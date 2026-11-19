use std::sync::{Arc, Mutex};

use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::provider::{ActionError, Connections, Provider, Snapshot};
use crate::settings::SettingsUpdate;
use crate::sync_check::{SyncCheck, SyncCheckError};
use crate::webdav::{ApplyError, Recovery, WebDav, WebDavError, WebDavStatus};

#[derive(Clone)]
struct Dashboard {
    connections: Arc<Connections>,
    webdav: Arc<WebDav>,
    sync_check: Arc<SyncCheck>,
    cloud_operation: Arc<Semaphore>,
    actions: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct LocalAddress {
    host: String,
    origin: String,
}

pub fn router(
    host: String,
    connections: Arc<Connections>,
    webdav: Arc<WebDav>,
    sync_check: Arc<SyncCheck>,
) -> Router {
    let address = LocalAddress {
        origin: format!("http://{host}"),
        host,
    };
    Router::new()
        .route(
            "/",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("dashboard/index.html"),
                )
            }),
        )
        .route(
            "/dashboard.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("dashboard/dashboard.js"),
                )
            }),
        )
        .route(
            "/dashboard.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("dashboard/dashboard.css"),
                )
            }),
        )
        .route("/api/connection", get(snapshot))
        .route("/api/connections", post(begin_connect))
        .route("/api/connections/{session_id}", get(connect_status))
        .route("/api/connections/{session_id}/answer", post(answer_connect))
        .route("/api/connections/{session_id}/cancel", post(cancel_connect))
        .route("/api/webdav", get(webdav_status).post(enable_webdav))
        .route("/api/webdav/stop", post(stop_webdav))
        .route("/api/settings", get(webdav_settings).post(apply_settings))
        .route("/api/sync-objects", post(sync_objects))
        .with_state(Dashboard {
            connections,
            webdav,
            sync_check,
            cloud_operation: Arc::new(Semaphore::new(1)),
            actions: Arc::new(Mutex::new(())),
        })
        .layer(middleware::from_fn_with_state(address, local_only))
}

async fn local_only(State(address): State<LocalAddress>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        != Some(&address.host)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.method() == Method::POST
        && headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            != Some(&address.origin)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, content_type)], body)
}

async fn snapshot(State(dashboard): State<Dashboard>) -> Json<Snapshot> {
    Json(dashboard.connections.snapshot())
}

async fn webdav_status(State(dashboard): State<Dashboard>) -> Json<WebDavStatus> {
    Json(dashboard.webdav.status())
}

async fn enable_webdav(State(dashboard): State<Dashboard>) -> Response {
    let _action = dashboard.actions.lock().unwrap();
    let connection = dashboard.connections.snapshot();
    if connection.connecting() {
        return conflict("Finish connecting the cloud provider before enabling WebDAV.");
    }
    let Some(remote) = connection.active else {
        return conflict("Connect a cloud provider before enabling WebDAV.");
    };
    if dashboard.cloud_operation.available_permits() == 0 {
        return conflict("Wait for the current cloud operation before enabling WebDAV.");
    }
    let result = dashboard.webdav.enable(remote.id);
    let code = if matches!(result, WebDavStatus::Starting) {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    (code, Json(result)).into_response()
}

async fn stop_webdav(State(dashboard): State<Dashboard>) -> Response {
    match tokio::task::spawn_blocking(move || {
        let _action = dashboard.actions.lock().unwrap();
        if dashboard.cloud_operation.available_permits() == 0 {
            return conflict("Wait for the current cloud operation before stopping WebDAV.");
        }
        match dashboard.webdav.stop() {
            Some(status) => Json(status).into_response(),
            None => conflict("Wait for WebDAV to finish starting or applying settings."),
        }
    })
    .await
    {
        Ok(response) => response,
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": "Could not stop WebDAV." })),
        )
            .into_response(),
    }
}

fn conflict(message: &'static str) -> Response {
    (StatusCode::CONFLICT, Json(json!({ "message": message }))).into_response()
}

async fn webdav_settings(State(dashboard): State<Dashboard>) -> Response {
    let Some(remote) = dashboard.connections.snapshot().active else {
        return conflict("Connect a cloud provider before opening Advanced.");
    };
    match dashboard.webdav.settings(remote.id) {
        Ok(settings) => Json(settings).into_response(),
        Err(error) => ApplyError::from(error).into_response(),
    }
}

async fn apply_settings(
    State(dashboard): State<Dashboard>,
    Json(update): Json<SettingsUpdate>,
) -> Response {
    let permit = {
        let _action = dashboard.actions.lock().unwrap();
        let connection = dashboard.connections.snapshot();
        if connection.connecting() {
            return conflict("Finish connecting the cloud provider before applying settings.");
        }
        if connection
            .active
            .is_none_or(|remote| remote.id != update.remote_id)
        {
            return conflict("The cloud connection changed. Reopen Advanced and try again.");
        }
        let Ok(permit) = Arc::clone(&dashboard.cloud_operation).try_acquire_owned() else {
            return conflict("Wait for the current cloud operation before applying settings.");
        };
        permit
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        dashboard.webdav.apply_settings(update)
    })
    .await
    {
        Ok(Ok(result)) => Json(result).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": "Could not apply WebDAV settings." })),
        )
            .into_response(),
    }
}

impl IntoResponse for ApplyError {
    fn into_response(self) -> Response {
        match self {
            Self::Busy => conflict("Wait for WebDAV to finish starting or applying settings."),
            Self::Invalid(message) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "message": message }))).into_response()
            }
            Self::Failed { error, recovery } => {
                let (status, message) = match &error {
                    WebDavError::SettingsRead => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Could not read the saved WebDAV settings.",
                    ),
                    WebDavError::SettingsSave => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Could not save the WebDAV settings.",
                    ),
                    WebDavError::PortInUse => (
                        StatusCode::CONFLICT,
                        "The selected WebDAV port is already in use.",
                    ),
                    WebDavError::PortUnavailable => (
                        StatusCode::BAD_REQUEST,
                        "Bridge could not bind the selected WebDAV port.",
                    ),
                    WebDavError::FolderCreate => (
                        StatusCode::BAD_GATEWAY,
                        "Could not prepare the selected cloud folder.",
                    ),
                    WebDavError::FolderAccess => (
                        StatusCode::BAD_GATEWAY,
                        "Could not access the selected cloud folder.",
                    ),
                    WebDavError::FolderTimeout => (
                        StatusCode::GATEWAY_TIMEOUT,
                        "Timed out while preparing the selected cloud folder.",
                    ),
                    _ => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Could not apply the WebDAV settings.",
                    ),
                };
                let recovery_message = match &recovery {
                    Recovery::NotNeeded => "",
                    Recovery::Restored => " The previous WebDAV endpoint is running again.",
                    Recovery::Failed(_) => {
                        " Bridge kept the previous settings, but WebDAV could not restart. Turn WebDAV on after resolving the error."
                    }
                };
                (
                    status,
                    Json(json!({
                        "message": format!("{message}{recovery_message}"),
                        "error": error,
                        "recovery": recovery,
                    })),
                )
                    .into_response()
            }
        }
    }
}

async fn sync_objects(State(dashboard): State<Dashboard>) -> Response {
    let (remote, permit) = {
        let _action = dashboard.actions.lock().unwrap();
        let connection = dashboard.connections.snapshot();
        if connection.connecting() {
            return conflict("Finish connecting the cloud provider before checking its folder.");
        }
        let Some(remote) = connection.active else {
            return conflict("Connect a cloud provider before checking its folder.");
        };
        let Ok(permit) = Arc::clone(&dashboard.cloud_operation).try_acquire_owned() else {
            return conflict("A cloud folder check or settings update is already running.");
        };
        (remote, permit)
    };

    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        dashboard.sync_check.list_sync_objects(remote.id)
    })
    .await;
    match result {
        Ok(Ok(listing)) => Json(listing).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => SyncCheckError::Unavailable.into_response(),
    }
}

impl IntoResponse for SyncCheckError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Settings => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings",
                "Could not read the WebDAV folder settings.",
            ),
            Self::Access => (
                StatusCode::BAD_GATEWAY,
                "access",
                "Could not access the cloud folder. Check that the folder exists and the cloud connection is active.",
            ),
            Self::TimedOut => (
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "Timed out while listing the cloud folder. Try checking again.",
            ),
            Self::InvalidListing => (
                StatusCode::BAD_GATEWAY,
                "invalid_listing",
                "rclone returned an invalid cloud listing.",
            ),
            Self::Unavailable => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "unavailable",
                "Could not run the cloud folder check.",
            ),
        };
        (status, Json(json!({ "code": code, "message": message }))).into_response()
    }
}

#[derive(Deserialize)]
struct ConnectRequest {
    provider: Provider,
}

async fn begin_connect(
    State(dashboard): State<Dashboard>,
    Json(request): Json<ConnectRequest>,
) -> Response {
    let _action = dashboard.actions.lock().unwrap();
    // The running process can update OAuth tokens in the dedicated config.
    if dashboard.webdav.status().active() {
        return conflict("Turn off WebDAV before changing the cloud provider.");
    }
    if dashboard.cloud_operation.available_permits() == 0 {
        return conflict("Wait for the current cloud operation before changing the provider.");
    }
    match dashboard.connections.begin_connect(request.provider) {
        Ok(snapshot) => (StatusCode::ACCEPTED, Json(snapshot)).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn connect_status(
    State(dashboard): State<Dashboard>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Snapshot>, ActionError> {
    dashboard.connections.connect_status(session_id).map(Json)
}

#[derive(Deserialize)]
struct AnswerRequest {
    question_id: Uuid,
    value: usize,
}

async fn answer_connect(
    State(dashboard): State<Dashboard>,
    Path(session_id): Path<Uuid>,
    Json(request): Json<AnswerRequest>,
) -> Result<Json<Snapshot>, ActionError> {
    dashboard
        .connections
        .answer_connect(session_id, request.question_id, request.value)
        .map(Json)
}

async fn cancel_connect(
    State(dashboard): State<Dashboard>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Snapshot>, ActionError> {
    dashboard.connections.cancel_connect(session_id).map(Json)
}

impl IntoResponse for ActionError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::SessionNotFound => (
                StatusCode::NOT_FOUND,
                "This setup session is no longer available.",
            ),
            Self::Busy => (StatusCode::CONFLICT, "A connection is already in progress."),
            Self::StaleQuestion => (StatusCode::CONFLICT, "This question has already changed."),
            Self::UnknownChoice => (
                StatusCode::BAD_REQUEST,
                "Select one of the available choices.",
            ),
            Self::StartFailed => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not start provider setup.",
            ),
        };
        (status, Json(json!({ "message": message }))).into_response()
    }
}
