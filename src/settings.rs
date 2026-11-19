use std::fs;
use std::io::{self, Write};
use std::num::NonZeroU16;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

pub const ZOTERO_FOLDER: &str = "zotero";
const DEFAULT_FOLDER: &str = "Zotero Cloud Bridge";
const DEFAULT_USER: &str = "zotero";

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", content = "number", rename_all = "snake_case")]
pub enum PortPreference {
    #[default]
    Auto,
    Preferred(NonZeroU16),
}

#[derive(Deserialize)]
pub struct SettingsUpdate {
    pub remote_id: String,
    cloud_folder: String,
    port: PortPreference,
    username: String,
    password: Option<String>,
}

#[derive(Serialize)]
pub struct SettingsView {
    remote_id: String,
    cloud_folder: String,
    port: PortPreference,
    last_port: Option<NonZeroU16>,
    username: String,
    password: Option<String>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
pub struct Settings {
    pub remote: String,
    pub folder: String,
    // Keep the existing on-disk field for the last port that actually served WebDAV.
    pub port: u16,
    #[serde(default)]
    pub port_preference: PortPreference,
    pub username: String,
    pub password: String,
}

pub fn file_path(config: &Path) -> PathBuf {
    config.with_file_name("webdav.json")
}

pub fn attachment_path(folder: &str) -> String {
    format!("{folder}/{ZOTERO_FOLDER}")
}

pub fn configured_attachment_path(config: &Path) -> io::Result<String> {
    let settings = Settings::read(&file_path(config))?;
    let folder = settings
        .as_ref()
        .map_or(DEFAULT_FOLDER, |value| value.folder.as_str());
    Ok(attachment_path(folder))
}

impl Settings {
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read(path) {
            Ok(contents) => {
                let settings: Self = serde_json::from_slice(&contents)?;
                if settings.folder.is_empty()
                    || settings.username.is_empty()
                    || settings.password.is_empty()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Incomplete WebDAV settings",
                    ));
                }
                Ok(Some(settings))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn defaults(remote: String) -> Result<Self, getrandom::Error> {
        Ok(Self {
            remote,
            folder: DEFAULT_FOLDER.into(),
            port: 0,
            port_preference: PortPreference::Auto,
            username: DEFAULT_USER.into(),
            password: generate_password()?,
        })
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut file = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o600))
            .tempfile_in(path.parent().unwrap())?;
        serde_json::to_writer(file.as_file_mut(), self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }

    pub fn view(remote_id: String, settings: Option<&Self>) -> SettingsView {
        match settings {
            Some(value) => SettingsView {
                remote_id,
                cloud_folder: value.folder.clone(),
                port: value.port_preference,
                last_port: NonZeroU16::new(value.port),
                username: value.username.clone(),
                password: Some(value.password.clone()),
            },
            None => SettingsView {
                remote_id,
                cloud_folder: DEFAULT_FOLDER.into(),
                port: PortPreference::Auto,
                last_port: None,
                username: DEFAULT_USER.into(),
                password: None,
            },
        }
    }
}

impl SettingsUpdate {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.cloud_folder.contains(['\\', ':'])
            || self.cloud_folder.chars().any(char::is_control)
            || self
                .cloud_folder
                .split('/')
                .any(|part| part.trim().is_empty() || matches!(part, "." | ".."))
        {
            return Err(
                "Enter a relative cloud folder such as Zotero Cloud Bridge or Library/Attachments.",
            );
        }
        if self.username.trim().is_empty()
            || self.username.contains(':')
            || self.username.chars().any(char::is_control)
        {
            return Err("Enter a username without colons or control characters.");
        }
        if self
            .password
            .as_ref()
            .is_some_and(|value| value.chars().any(char::is_control))
        {
            return Err("Enter a password without control characters.");
        }
        Ok(())
    }

    pub fn into_settings(self, previous: Option<&Settings>) -> Result<Settings, getrandom::Error> {
        let password = match self.password.filter(|value| !value.is_empty()) {
            Some(value) => value,
            None => match previous {
                Some(value) => value.password.clone(),
                None => generate_password()?,
            },
        };
        Ok(Settings {
            remote: self.remote_id,
            folder: self.cloud_folder,
            port: previous.map_or(0, |value| value.port),
            port_preference: self.port,
            username: self.username,
            password,
        })
    }
}

fn generate_password() -> Result<String, getrandom::Error> {
    let mut random = [0; 32];
    getrandom::fill(&mut random)?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_settings_keep_the_saved_port_in_automatic_mode() {
        let settings: Settings = serde_json::from_str(
            r#"{"remote":"bridge-example","folder":"Existing","port":51010,"username":"zotero","password":"test"}"#,
        )
        .unwrap();
        assert!(matches!(settings.port_preference, PortPreference::Auto));
        assert_eq!(settings.port, 51010);
        assert_eq!(settings.folder, "Existing");
    }
}
