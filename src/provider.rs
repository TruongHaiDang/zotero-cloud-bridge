use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, BufRead, BufReader};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use uuid::Uuid;

use crate::rclone::{self, ConfigOption, RunError};

const SESSION_TIMEOUT: Duration = Duration::from_secs(600);
const ACCESS_TIMEOUT: Duration = Duration::from_secs(45);
const CONFIG_FILE: &str = "rclone.conf";
const SESSION_PREFIX: &str = ".connect-";

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    GoogleDrive,
    OneDrive,
}

impl Provider {
    fn backend(self) -> &'static str {
        match self {
            Self::GoogleDrive => "drive",
            Self::OneDrive => "onedrive",
        }
    }
}

#[derive(Clone, Serialize)]
pub struct Remote {
    pub id: String,
    pub provider: Provider,
}

#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub active: Option<Remote>,
    session: Option<Session>,
}

impl Snapshot {
    pub fn connecting(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !session.phase.finished())
    }
}

#[derive(Clone, Serialize)]
struct Session {
    id: Uuid,
    provider: Provider,
    #[serde(flatten)]
    phase: Phase,
}

#[derive(Clone, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Phase {
    Preparing,
    WaitingForAuthorization,
    Question { question: Question },
    VerifyingAccess,
    Cancelling,
    Cancelled,
    Connected,
    Failed { message: &'static str },
    ReconnectRequired { message: &'static str },
}

impl Phase {
    fn finished(&self) -> bool {
        matches!(
            self,
            Self::Cancelled
                | Self::Connected
                | Self::Failed { .. }
                | Self::ReconnectRequired { .. }
        )
    }
}

#[derive(Clone, Serialize)]
struct Question {
    id: Uuid,
    prompt: String,
    choices: Vec<Choice>,
}

#[derive(Clone, Serialize)]
struct Choice {
    id: usize,
    label: String,
}

struct State {
    snapshot: Snapshot,
    answer: Option<usize>,
}

struct Worker {
    state: Mutex<State>,
    changed: Condvar,
    directory: PathBuf,
    binary: PathBuf,
}

pub struct Connections {
    worker: Arc<Worker>,
    task: Mutex<Option<JoinHandle<()>>>,
    _lock: File,
}

pub enum ActionError {
    SessionNotFound,
    Busy,
    StaleQuestion,
    UnknownChoice,
    StartFailed,
}

impl Connections {
    pub fn open(directory: PathBuf, binary: PathBuf) -> io::Result<Self> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        if !fs::symlink_metadata(&directory)?.is_dir() {
            return Err(io::Error::other(
                "Bridge's configuration directory must be a directory",
            ));
        }
        fs::set_permissions(&directory, Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join(".lock"))?;
        lock.try_lock().map_err(|error| {
            io::Error::other(format!(
                "Cannot lock Bridge's configuration directory ({error})"
            ))
        })?;

        let config = directory.join(CONFIG_FILE);
        let active = match fs::symlink_metadata(&config) {
            Ok(metadata) if metadata.is_file() => {
                fs::set_permissions(&config, Permissions::from_mode(0o600))?;
                Some(read_remote(&config)?)
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "Bridge's rclone configuration must be a file",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(SESSION_PREFIX.as_bytes())
                && entry.file_type()?.is_dir()
            {
                fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(Self {
            worker: Arc::new(Worker {
                state: Mutex::new(State {
                    snapshot: Snapshot {
                        active,
                        session: None,
                    },
                    answer: None,
                }),
                changed: Condvar::new(),
                directory,
                binary,
            }),
            task: Mutex::new(None),
            _lock: lock,
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.worker.state.lock().unwrap().snapshot.clone()
    }

    pub fn config_path(&self) -> PathBuf {
        self.worker.directory.join(CONFIG_FILE)
    }

    pub fn begin_connect(&self, provider: Provider) -> Result<Snapshot, ActionError> {
        let mut task = self.task.lock().unwrap();
        let mut state = self.worker.state.lock().unwrap();
        if state.snapshot.connecting() {
            return Err(ActionError::Busy);
        }
        if let Some(previous) = task.take() {
            let _ = previous.join();
        }
        let id = Uuid::new_v4();
        let deadline = Instant::now() + SESSION_TIMEOUT;
        let worker = Arc::clone(&self.worker);
        *task = Some(
            thread::Builder::new()
                .name("rclone-connect".into())
                .spawn(move || worker.connect(provider, deadline))
                .map_err(|_| ActionError::StartFailed)?,
        );
        state.snapshot.session = Some(Session {
            id,
            provider,
            phase: Phase::Preparing,
        });
        Ok(state.snapshot.clone())
    }

    pub fn connect_status(&self, session_id: Uuid) -> Result<Snapshot, ActionError> {
        let snapshot = self.snapshot();
        if snapshot
            .session
            .as_ref()
            .is_some_and(|session| session.id == session_id)
        {
            Ok(snapshot)
        } else {
            Err(ActionError::SessionNotFound)
        }
    }

    pub fn answer_connect(
        &self,
        session_id: Uuid,
        question_id: Uuid,
        value: usize,
    ) -> Result<Snapshot, ActionError> {
        let mut state = self.worker.state.lock().unwrap();
        let session = state.session(session_id)?;
        let Phase::Question { question } = &session.phase else {
            return Err(ActionError::StaleQuestion);
        };
        if question.id != question_id {
            return Err(ActionError::StaleQuestion);
        }
        if !question.choices.iter().any(|choice| choice.id == value) {
            return Err(ActionError::UnknownChoice);
        }
        session.phase = Phase::Preparing;
        state.answer = Some(value);
        self.worker.changed.notify_one();
        Ok(state.snapshot.clone())
    }

    pub fn cancel_connect(&self, session_id: Uuid) -> Result<Snapshot, ActionError> {
        let mut state = self.worker.state.lock().unwrap();
        let session = state.session(session_id)?;
        if !session.phase.finished() {
            session.phase = Phase::Cancelling;
            state.answer = None;
            self.worker.changed.notify_one();
        }
        Ok(state.snapshot.clone())
    }

    pub fn shutdown(&self) {
        let mut task = self.task.lock().unwrap();
        if let Some(id) = self.snapshot().session.map(|session| session.id) {
            let _ = self.cancel_connect(id);
        }
        if let Some(task) = task.take() {
            let _ = task.join();
        }
    }
}

impl State {
    fn session(&mut self, id: Uuid) -> Result<&mut Session, ActionError> {
        self.snapshot
            .session
            .as_mut()
            .filter(|session| session.id == id)
            .ok_or(ActionError::SessionNotFound)
    }

    fn phase(&mut self) -> &mut Phase {
        &mut self.snapshot.session.as_mut().unwrap().phase
    }
}

impl Worker {
    fn connect(&self, provider: Provider, deadline: Instant) {
        let result = self.stage(provider, deadline);
        let mut state = self.state.lock().unwrap();
        if matches!(state.phase(), Phase::Cancelling) {
            drop(result);
            *state.phase() = Phase::Cancelled;
            return;
        }

        *state.phase() = match result {
            Ok((stage, remote)) if Instant::now() < deadline => {
                match self.persist(&stage, &remote) {
                    Ok(()) => {
                        state.snapshot.active = Some(remote);
                        Phase::Connected
                    }
                    Err(_) => Phase::Failed {
                        message: "Could not save the verified cloud connection.",
                    },
                }
            }
            Ok(_) => failure(RunError::TimedOut, Step::Verify),
            Err(phase) => phase,
        };
        state.answer = None;
    }

    fn stage(&self, provider: Provider, deadline: Instant) -> Result<(TempDir, Remote), Phase> {
        let stage = tempfile::Builder::new()
            .prefix(SESSION_PREFIX)
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(&self.directory)
            .map_err(|_| Phase::Failed {
                message: "Could not create a private setup directory.",
            })?;
        let config = stage.path().join(CONFIG_FILE);
        let remote = Remote {
            id: format!("bridge-{}", Uuid::new_v4()),
            provider,
        };
        let mut continuation: Option<(String, String)> = None;
        let mut selected_drive = None;
        let mut step = Step::Configure;
        loop {
            let response = rclone::configure(
                &self.binary,
                &config,
                &remote.id,
                provider.backend(),
                continuation
                    .as_ref()
                    .map(|(state, result)| (state.as_str(), result.as_str())),
                deadline,
                || self.cancelled(),
            )
            .map_err(|error| failure(error, step))?;

            if !response.error.is_empty() {
                return Err(failure(RunError::Failed(response.error), step));
            }
            if response.state.is_empty() {
                break;
            }
            let option = response.option.ok_or_else(unexpected_step)?;
            let result = match (provider, option.name.as_str()) {
                (_, "config_is_local") => {
                    self.set_phase(Phase::WaitingForAuthorization)?;
                    step = Step::Authorize;
                    option_value(&option, "true")?
                }
                (Provider::GoogleDrive, "config_shared_client_id") => {
                    let prompt = "rclone’s shared Google Drive client ID is being retired during 2026. Use the shared client ID?";
                    let answer = self.ask(prompt, &option, deadline)?;
                    if answer.value == "false" {
                        return Err(Phase::Cancelled);
                    }
                    answer.value.clone()
                }
                (Provider::GoogleDrive, "config_change_team_drive") => {
                    option_value(&option, "false")?
                }
                (Provider::OneDrive, "config_type") => option_value(&option, "onedrive")?,
                (Provider::OneDrive, "config_driveid") => {
                    let answer = self.ask("Select the OneDrive to use.", &option, deadline)?;
                    selected_drive = Some(answer.help.clone());
                    answer.value.clone()
                }
                (Provider::OneDrive, "config_drive_ok") => {
                    let drive = selected_drive.as_deref().ok_or_else(unexpected_step)?;
                    self.ask(&format!("Use this OneDrive? {drive}"), &option, deadline)?
                        .value
                        .clone()
                }
                _ => return Err(unexpected_step()),
            };
            continuation = Some((response.state, result));
            if option.name != "config_is_local" {
                step = Step::Configure;
                self.set_phase(Phase::Preparing)?;
            }
        }

        self.set_phase(Phase::VerifyingAccess)?;
        rclone::verify_access(
            &self.binary,
            &config,
            &remote.id,
            deadline.min(Instant::now() + ACCESS_TIMEOUT),
            || self.cancelled(),
        )
        .map_err(|error| failure(error, Step::Verify))?;
        Ok((stage, remote))
    }

    fn persist(&self, stage: &TempDir, remote: &Remote) -> io::Result<()> {
        let source = stage.path().join(CONFIG_FILE);
        let saved = read_remote(&source)?;
        if saved.id != remote.id || saved.provider != remote.provider {
            return Err(io::Error::other(
                "Unexpected remote in staged configuration",
            ));
        }
        fs::set_permissions(&source, Permissions::from_mode(0o600))?;
        File::open(&source)?.sync_all()?;
        fs::rename(source, self.directory.join(CONFIG_FILE))
    }

    fn cancelled(&self) -> bool {
        matches!(self.state.lock().unwrap().phase(), Phase::Cancelling)
    }

    fn set_phase(&self, phase: Phase) -> Result<(), Phase> {
        let mut state = self.state.lock().unwrap();
        if matches!(state.phase(), Phase::Cancelling) {
            return Err(Phase::Cancelled);
        }
        *state.phase() = phase;
        Ok(())
    }

    fn ask<'a>(
        &self,
        prompt: &str,
        option: &'a ConfigOption,
        deadline: Instant,
    ) -> Result<&'a rclone::Example, Phase> {
        if option.examples.is_empty() {
            return Err(unexpected_step());
        }
        let question = Question {
            id: Uuid::new_v4(),
            prompt: prompt.into(),
            choices: option
                .examples
                .iter()
                .enumerate()
                .map(|(id, example)| Choice {
                    id,
                    label: example.help.clone(),
                })
                .collect(),
        };
        let mut state = self.state.lock().unwrap();
        if matches!(state.phase(), Phase::Cancelling) {
            return Err(Phase::Cancelled);
        }
        *state.phase() = Phase::Question { question };
        loop {
            if matches!(state.phase(), Phase::Cancelling) {
                return Err(Phase::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failure(RunError::TimedOut, Step::Configure));
            }
            if let Some(answer) = state.answer.take() {
                return Ok(&option.examples[answer]);
            }
            (state, _) = self.changed.wait_timeout(state, remaining).unwrap();
        }
    }
}

fn read_remote(config: &Path) -> io::Result<Remote> {
    let mut remote_id = None;
    for line in BufReader::new(File::open(config)?).lines() {
        let line = line?;
        let line = line.trim();
        if let Some(id) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            remote_id = id
                .strip_prefix("bridge-")
                .filter(|suffix| Uuid::parse_str(suffix).is_ok())
                .map(|_| id.to_owned());
        } else if let Some((key, value)) = line.split_once('=')
            && key.trim() == "type"
            && let Some(id) = remote_id.take()
        {
            let provider = match value.trim() {
                "drive" => Provider::GoogleDrive,
                "onedrive" => Provider::OneDrive,
                _ => break,
            };
            return Ok(Remote { id, provider });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Could not read Bridge's cloud connection",
    ))
}

fn option_value(option: &ConfigOption, value: &str) -> Result<String, Phase> {
    option
        .examples
        .iter()
        .find(|example| example.value == value)
        .map(|example| example.value.clone())
        .ok_or_else(unexpected_step)
}

fn unexpected_step() -> Phase {
    Phase::Failed {
        message: "rclone returned an unexpected setup step.",
    }
}

#[derive(Clone, Copy)]
enum Step {
    Configure,
    Authorize,
    Verify,
}

fn failure(error: RunError, step: Step) -> Phase {
    let message = match error {
        RunError::Cancelled => return Phase::Cancelled,
        RunError::TimedOut => match step {
            Step::Verify => "The access check timed out. Try connecting again.",
            _ => "The setup session expired. Try connecting again.",
        },
        RunError::Protocol => "rclone returned an invalid setup response.",
        RunError::Io => "Could not run rclone. Check that the rclone binary is available.",
        RunError::Failed(detail) => {
            let detail = detail.to_ascii_lowercase();
            if [
                "invalid_grant",
                "interaction_required",
                "rclone config reconnect",
                "token has been expired or revoked",
            ]
            .iter()
            .any(|indicator| detail.contains(indicator))
            {
                return Phase::ReconnectRequired {
                    message: "The provider requires new authorization. Connect again.",
                };
            }
            match step {
                Step::Configure => "rclone could not configure the selected provider.",
                Step::Authorize => "Authorization did not complete. Try connecting again.",
                Step::Verify => {
                    "Could not access the selected drive. Check your connection and try again."
                }
            }
        }
    };
    Phase::Failed { message }
}
