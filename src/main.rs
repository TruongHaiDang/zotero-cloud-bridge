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

#[cfg(target_os = "macos")]
static MENU_QUIT: tokio::sync::Notify = tokio::sync::Notify::const_new();

#[cfg(target_os = "macos")]
extern "C" fn request_menu_quit() {
    MENU_QUIT.notify_one();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn bridge_run_menu_bar(request_quit: extern "C" fn());
        }

        std::thread::spawn(|| {
            let code = match run_bridge() {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("Zotero Cloud Bridge: {error}");
                    1
                }
            };
            std::process::exit(code);
        });
        // AppKit's event loop must run on the main thread.
        unsafe { bridge_run_menu_bar(request_menu_quit) };
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    run_bridge()
}

#[tokio::main]
async fn run_bridge() -> Result<(), Box<dyn std::error::Error>> {
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
    #[cfg(target_os = "macos")]
    tokio::select! {
        _ = MENU_QUIT.notified() => {},
        _ = terminal_shutdown_signal() => {},
    }
    #[cfg(not(target_os = "macos"))]
    terminal_shutdown_signal().await;
}

async fn terminal_shutdown_signal() {
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
