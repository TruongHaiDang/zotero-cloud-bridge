mod dashboard;
mod provider;
mod rclone;
mod settings;
mod sync_check;
mod webdav;

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| io::Error::other("HOME is required to locate Bridge's configuration"))?;
    let directory = PathBuf::from(home).join("Library/Application Support/Zotero Cloud Bridge");
    let binary = rclone::binary()?;
    let connections = Arc::new(provider::Connections::open(directory, binary.clone())?);
    let config = connections.config_path();
    let webdav = Arc::new(webdav::WebDav::new(binary.clone(), config.clone())?);
    let sync_check = Arc::new(sync_check::SyncCheck::new(binary, config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?.to_string();
    let url = format!("http://{address}");
    println!("Zotero Cloud Bridge: {url}");

    if cfg!(target_os = "macos") && !std::env::args_os().any(|arg| arg == "--no-browser") {
        let _ = Command::new("/usr/bin/open")
            .arg(&url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let result = axum::serve(
        listener,
        dashboard::router(
            address,
            Arc::clone(&connections),
            Arc::clone(&webdav),
            sync_check,
        ),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;
    webdav.shutdown();
    connections.shutdown();
    result?;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    if let Ok(mut terminate) = signal(SignalKind::terminate()) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    } else {
        let _ = tokio::signal::ctrl_c().await;
    }
}
