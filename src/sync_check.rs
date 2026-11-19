use std::io::BufReader;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::rclone::{self, RunError};
use crate::settings;

const LIST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize, Serialize)]
pub struct RemoteObject {
    #[serde(rename(deserialize = "Path"))]
    path: String,
    #[serde(rename(deserialize = "Size"), deserialize_with = "object_size")]
    size_bytes: Option<u64>,
}

fn object_size<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    // rclone uses -1 when the provider cannot report a size.
    i64::deserialize(deserializer).map(|size| u64::try_from(size).ok())
}

#[derive(Serialize)]
pub struct CloudListing {
    remote_id: String,
    folder: String,
    objects: Vec<RemoteObject>,
}

pub enum SyncCheckError {
    Settings,
    Access,
    TimedOut,
    InvalidListing,
    Unavailable,
}

pub struct SyncCheck {
    binary: PathBuf,
    config: PathBuf,
}

impl SyncCheck {
    pub fn new(binary: PathBuf, config: PathBuf) -> Self {
        Self { binary, config }
    }

    pub fn list_sync_objects(&self, remote_id: String) -> Result<CloudListing, SyncCheckError> {
        let folder = settings::configured_attachment_path(&self.config)
            .map_err(|_| SyncCheckError::Settings)?;
        let target = format!("{remote_id}:{folder}");
        let output = rclone::list_files(
            &self.binary,
            &self.config,
            &target,
            Instant::now() + LIST_TIMEOUT,
        )
        .map_err(|error| match error {
            RunError::TimedOut => SyncCheckError::TimedOut,
            RunError::Failed(_) => SyncCheckError::Access,
            _ => SyncCheckError::Unavailable,
        })?;
        let mut objects: Vec<RemoteObject> = serde_json::from_reader(BufReader::new(output))
            .map_err(|_| SyncCheckError::InvalidListing)?;
        objects.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(CloudListing {
            remote_id,
            folder,
            objects,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn lists_unknown_sizes_without_losing_other_objects() {
        let objects: Vec<RemoteObject> = serde_json::from_value(json!([
            {"Path": "attachment.zip", "Size": 42},
            {"Path": "notes.docx", "Size": -1}
        ]))
        .unwrap();
        assert_eq!(
            to_value(objects).unwrap(),
            json!([
                {"path": "attachment.zip", "size_bytes": 42},
                {"path": "notes.docx", "size_bytes": null}
            ])
        );
    }
}
