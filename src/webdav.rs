use std::io;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;

use crate::rclone::{self, RunError, WebDavChild};
use crate::settings::{
    self, PortPreference, Settings, SettingsUpdate, SettingsView, ZOTERO_FOLDER,
};

const FOLDER_TIMEOUT: Duration = Duration::from_secs(45);
const START_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WebDavStatus {
    Stopped,
    Starting,
    Applying,
    Running { endpoint: Endpoint },
    Failed { error: WebDavError },
}

impl WebDavStatus {
    pub fn active(&self) -> bool {
        matches!(self, Self::Starting | Self::Applying | Self::Running { .. })
    }
}

#[derive(Serialize)]
pub struct Endpoint {
    url: String,
    username: String,
    password: String,
    port_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum WebDavError {
    SettingsRead,
    SettingsSave,
    PasswordGeneration,
    FolderCreate,
    FolderAccess,
    FolderTimeout,
    Launch,
    PortInUse,
    PortUnavailable,
    ReadinessTimeout,
    CredentialsRejected,
    ReadinessProbe,
    Exited(String),
    Monitor,
    Cancelled,
}

#[derive(Serialize)]
pub struct ApplyOutcome {
    settings: SettingsView,
    webdav: WebDavStatus,
    update_zotero: bool,
}

pub enum ApplyError {
    Busy,
    Invalid(&'static str),
    Failed {
        error: WebDavError,
        recovery: Recovery,
    },
}

impl From<WebDavError> for ApplyError {
    fn from(error: WebDavError) -> Self {
        Self::Failed {
            error,
            recovery: Recovery::NotNeeded,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "status", content = "error", rename_all = "snake_case")]
pub enum Recovery {
    NotNeeded,
    Restored,
    Failed(WebDavError),
}

enum ServiceState {
    Stopped,
    Starting,
    Applying,
    Running(Running),
    Failed(WebDavError),
}

struct Running {
    process: WebDavChild,
    settings: Settings,
    port_changed: bool,
}

impl ServiceState {
    fn snapshot(&self) -> WebDavStatus {
        match self {
            Self::Stopped => WebDavStatus::Stopped,
            Self::Starting => WebDavStatus::Starting,
            Self::Applying => WebDavStatus::Applying,
            Self::Failed(error) => WebDavStatus::Failed {
                error: error.clone(),
            },
            Self::Running(running) => WebDavStatus::Running {
                endpoint: Endpoint {
                    url: endpoint_url(running.settings.port),
                    username: running.settings.username.clone(),
                    password: running.settings.password.clone(),
                    port_changed: running.port_changed,
                },
            },
        }
    }
}

struct Worker {
    binary: PathBuf,
    config: PathBuf,
    settings: PathBuf,
    state: Mutex<ServiceState>,
    shutdown: AtomicBool,
}

pub struct WebDav {
    worker: Arc<Worker>,
    operation: Mutex<Option<JoinHandle<()>>>,
    monitor: Mutex<Option<JoinHandle<()>>>,
}

impl WebDav {
    pub fn new(binary: PathBuf, config: PathBuf) -> io::Result<Self> {
        let worker = Arc::new(Worker {
            binary,
            settings: settings::file_path(&config),
            config,
            state: Mutex::new(ServiceState::Stopped),
            shutdown: AtomicBool::new(false),
        });
        let watch = Arc::clone(&worker);
        let monitor = thread::Builder::new()
            .name("webdav-monitor".into())
            .spawn(move || watch.monitor())?;
        Ok(Self {
            worker,
            operation: Mutex::new(None),
            monitor: Mutex::new(Some(monitor)),
        })
    }

    pub fn status(&self) -> WebDavStatus {
        self.worker.state.lock().unwrap().snapshot()
    }

    pub fn settings(&self, remote: String) -> Result<SettingsView, WebDavError> {
        let saved = self.worker.read_settings()?;
        Ok(Settings::view(remote, saved.as_ref()))
    }

    pub fn enable(&self, remote: String) -> WebDavStatus {
        let Ok(mut operation) = self.operation.try_lock() else {
            return self.status();
        };
        let mut state = self.worker.state.lock().unwrap();
        if self.worker.cancelled()
            || matches!(
                *state,
                ServiceState::Starting | ServiceState::Applying | ServiceState::Running(_)
            )
        {
            return state.snapshot();
        }
        join(&mut operation);
        let worker = Arc::clone(&self.worker);
        match thread::Builder::new()
            .name("webdav-start".into())
            .spawn(move || worker.start(remote))
        {
            Ok(task) => {
                *operation = Some(task);
                *state = ServiceState::Starting;
            }
            Err(_) => *state = ServiceState::Failed(WebDavError::Launch),
        }
        state.snapshot()
    }

    pub fn apply_settings(&self, update: SettingsUpdate) -> Result<ApplyOutcome, ApplyError> {
        let Ok(mut operation) = self.operation.try_lock() else {
            return Err(ApplyError::Busy);
        };
        if self.worker.cancelled() || matches!(self.status(), WebDavStatus::Starting) {
            return Err(ApplyError::Busy);
        }
        join(&mut operation);
        self.worker.apply(update)
    }

    pub fn stop(&self) -> Option<WebDavStatus> {
        let _operation = self.operation.try_lock().ok()?;
        let mut state = self.worker.state.lock().unwrap();
        if matches!(*state, ServiceState::Starting | ServiceState::Applying) {
            return None;
        }
        *state = ServiceState::Stopped;
        Some(WebDavStatus::Stopped)
    }

    pub fn shutdown(&self) {
        self.worker.shutdown.store(true, Ordering::Relaxed);
        join(&mut self.operation.lock().unwrap());
        join(&mut self.monitor.lock().unwrap());
        *self.worker.state.lock().unwrap() = ServiceState::Stopped;
    }
}

impl Drop for WebDav {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn join(handle: &mut Option<JoinHandle<()>>) {
    if let Some(task) = handle.take() {
        let _ = task.join();
    }
}

fn endpoint_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

impl Worker {
    fn read_settings(&self) -> Result<Option<Settings>, WebDavError> {
        Settings::read(&self.settings).map_err(|_| WebDavError::SettingsRead)
    }

    fn start(&self, remote: String) {
        let result = (|| {
            let mut settings = match self.read_settings()? {
                Some(value) => value,
                None => Settings::defaults(remote.clone())
                    .map_err(|_| WebDavError::PasswordGeneration)?,
            };
            settings.remote = remote;
            self.prepare_folder(&settings)?;
            self.activate(settings)
        })();
        *self.state.lock().unwrap() = match result {
            Ok(running) => ServiceState::Running(running),
            Err(WebDavError::Cancelled) => ServiceState::Stopped,
            Err(error) => ServiceState::Failed(error),
        };
    }

    fn apply(&self, update: SettingsUpdate) -> Result<ApplyOutcome, ApplyError> {
        update.validate().map_err(ApplyError::Invalid)?;
        let previous = self.read_settings()?;
        let next = update
            .into_settings(previous.as_ref())
            .map_err(|_| WebDavError::PasswordGeneration)?;
        if previous.as_ref() == Some(&next) {
            return Ok(self.outcome(&next, false));
        }

        if let PortPreference::Preferred(port) = next.port_preference {
            let port = port.get();
            let owns_port = matches!(&*self.state.lock().unwrap(), ServiceState::Running(old) if old.settings.port == port);
            if !owns_port {
                TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
                    if error.kind() == io::ErrorKind::AddrInUse {
                        WebDavError::PortInUse
                    } else {
                        WebDavError::PortUnavailable
                    }
                })?;
            }
        }
        self.prepare_folder(&next)?;

        // Leave the current endpoint available until the new folder and port pass validation.
        let old = {
            let mut state = self.state.lock().unwrap();
            match std::mem::replace(&mut *state, ServiceState::Applying) {
                ServiceState::Running(old) => Some(old),
                other => {
                    *state = other;
                    None
                }
            }
        };
        let Some(old) = old else {
            let update_zotero = previous.as_ref().is_some_and(|old| {
                credentials_changed(old, &next)
                    || (old.port != 0 && matches!(next.port_preference, PortPreference::Preferred(port) if port.get() != old.port))
            });
            self.save(&next)?;
            return Ok(self.outcome(&next, update_zotero));
        };

        let Running {
            process,
            settings: old_settings,
            port_changed,
        } = old;
        drop(process);
        match self.activate(next) {
            Ok(running) => {
                let update_zotero = old_settings.port != running.settings.port
                    || credentials_changed(&old_settings, &running.settings);
                let settings =
                    Settings::view(running.settings.remote.clone(), Some(&running.settings));
                let state = ServiceState::Running(running);
                let webdav = state.snapshot();
                *self.state.lock().unwrap() = state;
                Ok(ApplyOutcome {
                    settings,
                    webdav,
                    update_zotero,
                })
            }
            Err(error) => {
                // The saved file still contains the previous settings. Restore the exact old port.
                let recovery = match self.ready(&old_settings, old_settings.port) {
                    Ok((process, _)) => {
                        *self.state.lock().unwrap() = ServiceState::Running(Running {
                            process,
                            settings: old_settings,
                            port_changed,
                        });
                        Recovery::Restored
                    }
                    Err(restore_error) => {
                        *self.state.lock().unwrap() = ServiceState::Failed(restore_error.clone());
                        Recovery::Failed(restore_error)
                    }
                };
                Err(ApplyError::Failed { error, recovery })
            }
        }
    }

    fn outcome(&self, settings: &Settings, update_zotero: bool) -> ApplyOutcome {
        ApplyOutcome {
            settings: Settings::view(settings.remote.clone(), Some(settings)),
            webdav: self.state.lock().unwrap().snapshot(),
            update_zotero,
        }
    }

    fn prepare_folder(&self, settings: &Settings) -> Result<(), WebDavError> {
        let folder = format!(
            "{}:{}",
            settings.remote,
            settings::attachment_path(&settings.folder)
        );
        let deadline = Instant::now() + FOLDER_TIMEOUT;
        for (command, error) in [
            ("mkdir", WebDavError::FolderCreate),
            ("lsd", WebDavError::FolderAccess),
        ] {
            rclone::run_folder_command(
                &self.binary,
                &self.config,
                command,
                &folder,
                deadline,
                || self.cancelled(),
            )
            .map_err(|failure| match failure {
                RunError::Cancelled => WebDavError::Cancelled,
                RunError::TimedOut => WebDavError::FolderTimeout,
                _ => error,
            })?;
        }
        Ok(())
    }

    fn activate(&self, mut settings: Settings) -> Result<Running, WebDavError> {
        let previous_port = settings.port;
        let (process, port) = match settings.port_preference {
            PortPreference::Auto => match self.ready(&settings, previous_port) {
                Err(WebDavError::PortInUse) if previous_port != 0 => self.ready(&settings, 0)?,
                result => result?,
            },
            PortPreference::Preferred(port) => self.ready(&settings, port.get())?,
        };
        settings.port = port;
        self.save(&settings)?;
        Ok(Running {
            process,
            settings,
            port_changed: previous_port != 0 && previous_port != port,
        })
    }

    fn save(&self, settings: &Settings) -> Result<(), WebDavError> {
        if self.cancelled() {
            return Err(WebDavError::Cancelled);
        }
        settings
            .save(&self.settings)
            .map_err(|_| WebDavError::SettingsSave)
    }

    fn ready(&self, settings: &Settings, port: u16) -> Result<(WebDavChild, u16), WebDavError> {
        let target = format!("{}:{}", settings.remote, settings.folder);
        let deadline = Instant::now() + START_TIMEOUT;
        if self.cancelled() {
            return Err(WebDavError::Cancelled);
        }
        let mut child = rclone::serve_webdav(
            &self.binary,
            &self.config,
            &target,
            port,
            &settings.username,
            &settings.password,
        )
        .map_err(|_| WebDavError::Launch)?;
        let actual_port = loop {
            self.check_starting(&mut child)?;
            if let Some(bound) = child.bound_port() {
                break bound;
            }
            if Instant::now() >= deadline {
                return Err(WebDavError::ReadinessTimeout);
            }
            thread::sleep(POLL_INTERVAL);
        };
        let readiness = check_readiness(
            &endpoint_url(actual_port),
            &settings.username,
            &settings.password,
        );
        self.check_starting(&mut child)?;
        readiness?;
        Ok((child, actual_port))
    }

    fn check_starting(&self, child: &mut WebDavChild) -> Result<(), WebDavError> {
        if self.cancelled() {
            return Err(WebDavError::Cancelled);
        }
        if let Some(status) = child.try_wait().map_err(|_| WebDavError::Monitor)? {
            if child.port_in_use() {
                return Err(WebDavError::PortInUse);
            }
            return Err(WebDavError::Exited(status.to_string()));
        }
        Ok(())
    }

    fn monitor(&self) {
        while !self.cancelled() {
            {
                let mut state = self.state.lock().unwrap();
                if let ServiceState::Running(running) = &mut *state {
                    match running.process.try_wait() {
                        Ok(Some(status)) if status.success() => *state = ServiceState::Stopped,
                        Ok(Some(status)) => {
                            *state = ServiceState::Failed(WebDavError::Exited(status.to_string()))
                        }
                        Err(_) => *state = ServiceState::Failed(WebDavError::Monitor),
                        Ok(None) => {}
                    }
                }
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn cancelled(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

fn credentials_changed(old: &Settings, new: &Settings) -> bool {
    old.username != new.username || old.password != new.password
}

fn check_readiness(url: &str, username: &str, password: &str) -> Result<(), WebDavError> {
    let auth = STANDARD.encode(format!("{username}:{password}"));
    let request = Request::builder()
        .method("PROPFIND")
        .uri(format!("{url}{ZOTERO_FOLDER}/"))
        .header(header::AUTHORIZATION, format!("Basic {auth}"))
        .header("Depth", "0")
        .body(())
        .map_err(|_| WebDavError::ReadinessProbe)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .allow_non_standard_methods(true)
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(PROBE_TIMEOUT))
        .build()
        .into();
    match agent.run(request) {
        Ok(response) if response.status() == StatusCode::MULTI_STATUS => Ok(()),
        Ok(response)
            if matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) =>
        {
            Err(WebDavError::CredentialsRejected)
        }
        Err(ureq::Error::Timeout(_)) => Err(WebDavError::ReadinessTimeout),
        _ => Err(WebDavError::ReadinessProbe),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::HeaderMap, routing::any};

    #[tokio::test]
    async fn readiness_requires_authenticated_propfind() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/zotero/",
            any(
                |method: axum::http::Method, headers: HeaderMap| async move {
                    assert_eq!(method.as_str(), "PROPFIND");
                    assert_eq!(headers["Depth"], "0");
                    if headers[header::AUTHORIZATION] == "Basic em90ZXJvOnRlc3Q=" {
                        StatusCode::MULTI_STATUS
                    } else {
                        StatusCode::UNAUTHORIZED
                    }
                },
            ),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::task::spawn_blocking(move || {
            assert_eq!(check_readiness(&url, "zotero", "test"), Ok(()));
            assert_eq!(
                check_readiness(&url, "zotero", "incorrect"),
                Err(WebDavError::CredentialsRejected)
            );
        })
        .await
        .unwrap();
        server.abort();
    }
}
