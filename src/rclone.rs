use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

const OUTPUT_LIMIT: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ConfigResponse {
    pub state: String,
    pub option: Option<ConfigOption>,
    pub error: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ConfigOption {
    pub name: String,
    pub examples: Vec<Example>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Example {
    pub value: String,
    pub help: String,
}

pub enum RunError {
    Cancelled,
    TimedOut,
    Io,
    Protocol,
    Failed(String),
}

pub fn binary() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let bundled = executable
        .parent()
        .filter(|directory| directory.ends_with("Contents/MacOS"))
        .and_then(Path::parent)
        .map(|contents| contents.join("Resources/rclone"));
    bundled
        .filter(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            }) && path.canonicalize().is_ok_and(|resolved| resolved == *path)
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Bundled rclone is missing or unusable. Build the macOS app with scripts/bundle-macos.sh.",
            )
        })
}

pub fn configure(
    binary: &Path,
    config: &Path,
    remote: &str,
    backend: &str,
    continuation: Option<(&str, &str)>,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<ConfigResponse, RunError> {
    let mut args = match continuation {
        Some((state, result)) => vec![
            "config".into(),
            "update".into(),
            remote.into(),
            "--continue".into(),
            format!("--state={state}").into(),
            format!("--result={result}").into(),
        ],
        None => vec![
            "config".into(),
            "create".into(),
            remote.into(),
            backend.into(),
            "--non-interactive".into(),
        ],
    };
    if backend == "drive" {
        args.push("scope=drive".into());
    }
    let mut stdout = run(binary, config, args, true, deadline, cancelled)?.ok_or(RunError::Io)?;
    let output = read_output(&mut stdout)?;
    serde_json::from_slice(&output).map_err(|_| RunError::Protocol)
}

pub fn verify_access(
    binary: &Path,
    config: &Path,
    remote: &str,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<(), RunError> {
    run_folder_command(
        binary,
        config,
        "lsd",
        &format!("{remote}:"),
        deadline,
        cancelled,
    )
}

pub fn run_folder_command(
    binary: &Path,
    config: &Path,
    action: &str,
    target: &str,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<(), RunError> {
    let args = vec![action.into(), target.into(), "--retries=1".into()];
    run(binary, config, args, false, deadline, cancelled).map(|_| ())
}

pub fn list_files(
    binary: &Path,
    config: &Path,
    target: &str,
    deadline: Instant,
) -> Result<File, RunError> {
    let args = [
        "lsjson",
        target,
        "--recursive",
        "--files-only",
        "--no-mimetype",
        "--no-modtime",
        "--retries=1",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let mut stdout = run(binary, config, args, true, deadline, || false)?.ok_or(RunError::Io)?;
    stdout.rewind().map_err(|_| RunError::Io)?;
    Ok(stdout)
}

#[derive(Default)]
struct ServeLog {
    port: Option<u16>,
    port_in_use: bool,
}

pub struct WebDavChild {
    child: Child,
    log: Arc<Mutex<ServeLog>>,
    reader: Option<JoinHandle<()>>,
}

impl WebDavChild {
    pub fn bound_port(&self) -> Option<u16> {
        self.log.lock().unwrap().port
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn port_in_use(&mut self) -> bool {
        // The child has exited. Drain its final log line before classifying the error.
        self.join_reader();
        self.log.lock().unwrap().port_in_use
    }

    fn join_reader(&mut self) {
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for WebDavChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join_reader();
    }
}

pub fn serve_webdav(
    binary: &Path,
    config: &Path,
    target: &str,
    port: u16,
    username: &str,
    password: &str,
) -> io::Result<WebDavChild> {
    let mut child = command(binary, config)
        .args(["serve", "webdav", target, "--addr"])
        .arg(format!("127.0.0.1:{port}"))
        .args(["--use-json-log", "--log-level=NOTICE"])
        .env_remove("LISTEN_PID")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_FDNAMES")
        .env("RCLONE_USER", username)
        .env("RCLONE_PASS", password)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().unwrap();
    let log = Arc::new(Mutex::new(ServeLog::default()));
    let mut process = WebDavChild {
        child,
        log: Arc::clone(&log),
        reader: None,
    };
    process.reader = Some(thread::Builder::new().name("rclone-webdav-log".into()).spawn(move || {
        #[derive(Deserialize)]
        struct LogEntry {
            msg: String,
        }

        let listen_error = format!("Failed to webdav: failed to init server: listen tcp 127.0.0.1:{port}: bind: address already in use");
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let Ok(entry) = serde_json::from_str::<LogEntry>(&line) else {
                continue;
            };
            if let Some(bound) = entry.msg
                .strip_prefix("WebDav Server started on [http://127.0.0.1:")
                .and_then(|port| port.strip_suffix("/]"))
                .and_then(|port| port.parse::<u16>().ok())
                .filter(|&bound| bound != 0 && (port == 0 || bound == port))
            {
                log.lock().unwrap().port = Some(bound);
            } else if entry.msg == listen_error {
                log.lock().unwrap().port_in_use = true;
            }
        }
    })?);
    Ok(process)
}

fn command(binary: &Path, config: &Path) -> Command {
    let mut command = Command::new(binary);
    command.arg("--config").arg(config).stdin(Stdio::null());
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"RCLONE_") {
            command.env_remove(name);
        }
    }
    command
}

fn run(
    binary: &Path,
    config: &Path,
    args: Vec<OsString>,
    capture_stdout: bool,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<Option<File>, RunError> {
    let directory = config.parent().ok_or(RunError::Io)?;
    let mut stderr = tempfile::tempfile_in(directory).map_err(|_| RunError::Io)?;
    let stdout = capture_stdout
        .then(|| tempfile::tempfile_in(directory))
        .transpose()
        .map_err(|_| RunError::Io)?;

    let mut command = command(binary, config);
    command
        .args(args)
        .stderr(stderr.try_clone().map_err(|_| RunError::Io)?)
        .stdout(match &stdout {
            Some(file) => Stdio::from(file.try_clone().map_err(|_| RunError::Io)?),
            None => Stdio::null(),
        });
    check_running(deadline, &cancelled)?;
    let mut child = command.spawn().map_err(|_| RunError::Io)?;
    let result = loop {
        match check_running(deadline, &cancelled)
            .and_then(|()| child.try_wait().map_err(|_| RunError::Io))
        {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error);
            }
        }
    }?;

    if !result.success() {
        let stderr = read_output(&mut stderr)?;
        return Err(RunError::Failed(
            String::from_utf8_lossy(&stderr).into_owned(),
        ));
    }
    Ok(stdout)
}

fn check_running(deadline: Instant, cancelled: impl Fn() -> bool) -> Result<(), RunError> {
    if cancelled() {
        return Err(RunError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(RunError::TimedOut);
    }
    Ok(())
}

fn read_output(file: &mut File) -> Result<Vec<u8>, RunError> {
    file.rewind().map_err(|_| RunError::Io)?;
    let mut output = Vec::new();
    file.take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut output)
        .map_err(|_| RunError::Io)?;
    if output.len() as u64 > OUTPUT_LIMIT {
        return Err(RunError::Protocol);
    }
    Ok(output)
}
