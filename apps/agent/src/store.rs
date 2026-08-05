use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use gflick_protocol as protocol;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const SETTINGS_VERSION: u32 = 1;
const SETTINGS_DIRECTORY: &str = "gflick";
const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ControlPreference {
    Host,
    Onboard { profile: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PollingPreference {
    Shared { hz: u16 },
    PerConnection { wired_hz: u16, wireless_hz: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BunnyHoppingPreference {
    pub enabled: bool,
    pub timeout_ms: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "control", rename_all = "snake_case")]
pub enum LightingPreference {
    Firmware,
    Software {
        zone: u8,
        #[serde(flatten)]
        effect: protocol::LightingEffect,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate: Option<PollingPreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_off_distance: Option<protocol::LiftOffDistance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_mode: Option<protocol::SurfaceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operating_mode: Option<protocol::OperatingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bunny_hopping: Option<BunnyHoppingPreference>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ControlPreference>,
    #[serde(default)]
    pub host: HostPreferences,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<LightingPreference>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsDocument {
    version: u32,
    #[serde(default)]
    devices: BTreeMap<String, DevicePreferences>,
}

#[derive(Deserialize)]
struct VersionHeader {
    version: u32,
}

impl Default for SettingsDocument {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            devices: BTreeMap::new(),
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    document: SettingsDocument,
}

impl SettingsStore {
    pub fn default_path() -> Result<PathBuf> {
        let base = BaseDirs::new().context("could not determine the per-user config directory")?;
        Ok(base
            .config_dir()
            .join(SETTINGS_DIRECTORY)
            .join(SETTINGS_FILE))
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let document = match fs::read(&path) {
            Ok(bytes) => match parse_document(&bytes) {
                Ok(document) => document,
                Err(LoadError::UnsupportedVersion(version)) => {
                    bail!(
                        "settings `{}` use unsupported schema version {}; this agent supports version {}",
                        path.display(),
                        version,
                        SETTINGS_VERSION
                    );
                }
                Err(LoadError::Invalid(error)) => {
                    let backup = quarantine_path(&path)?;
                    fs::rename(&path, &backup).with_context(|| {
                        format!(
                            "settings file `{}` is invalid and could not be preserved as `{}`",
                            path.display(),
                            backup.display()
                        )
                    })?;
                    eprintln!(
                        "Settings file `{}` was invalid ({error}); preserved it as `{}`",
                        path.display(),
                        backup.display()
                    );
                    SettingsDocument::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SettingsDocument::default()
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings `{}`", path.display()));
            }
        };

        Ok(Self { path, document })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn device(&self, hardware_id: &str) -> Option<&DevicePreferences> {
        self.document.devices.get(hardware_id)
    }

    pub fn device_mut(&mut self, hardware_id: &str) -> &mut DevicePreferences {
        self.document
            .devices
            .entry(hardware_id.to_owned())
            .or_default()
    }

    pub fn insert_if_missing(&mut self, hardware_id: &str, preferences: DevicePreferences) -> bool {
        if self.document.devices.contains_key(hardware_id) {
            return false;
        }
        self.document
            .devices
            .insert(hardware_id.to_owned(), preferences);
        true
    }

    pub fn save(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .context("settings path has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create settings directory `{}`", parent.display())
        })?;

        let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "failed to create a temporary settings file in `{}`",
                parent.display()
            )
        })?;
        serde_json::to_writer_pretty(&mut temporary, &self.document)
            .context("failed to serialize settings")?;
        temporary
            .write_all(b"\n")
            .context("failed to finish writing settings")?;
        temporary
            .as_file()
            .sync_all()
            .context("failed to flush settings to disk")?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "failed to atomically replace settings `{}`",
                    self.path.display()
                )
            })?;

        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| {
                format!("failed to flush settings directory `{}`", parent.display())
            })?;
        Ok(())
    }
}

enum LoadError {
    Invalid(serde_json::Error),
    UnsupportedVersion(u32),
}

fn parse_document(bytes: &[u8]) -> Result<SettingsDocument, LoadError> {
    let header = serde_json::from_slice::<VersionHeader>(bytes).map_err(LoadError::Invalid)?;
    if header.version != SETTINGS_VERSION {
        return Err(LoadError::UnsupportedVersion(header.version));
    }
    serde_json::from_slice(bytes).map_err(LoadError::Invalid)
}

fn quarantine_path(path: &Path) -> Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_nanos();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("settings filename is not valid UTF-8")?;
    Ok(path.with_file_name(format!("{file_name}.corrupt-{timestamp}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_preferences() -> DevicePreferences {
        DevicePreferences {
            control: Some(ControlPreference::Host),
            host: HostPreferences {
                dpi: Some(800),
                polling_rate: Some(PollingPreference::PerConnection {
                    wired_hz: 1000,
                    wireless_hz: 2000,
                }),
                lift_off_distance: Some(protocol::LiftOffDistance::High),
                surface_mode: Some(protocol::SurfaceMode::Off),
                operating_mode: None,
                bunny_hopping: Some(BunnyHoppingPreference {
                    enabled: false,
                    timeout_ms: 0,
                }),
            },
            lighting: None,
        }
    }

    #[test]
    fn atomically_round_trips_versioned_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path.clone()).unwrap();
        assert!(store.insert_if_missing("046d:unit:1077e69f", sample_preferences()));
        store.save().unwrap();

        let loaded = SettingsStore::load(path).unwrap();
        assert_eq!(
            loaded.device("046d:unit:1077e69f"),
            Some(&sample_preferences())
        );
    }

    #[test]
    fn preserves_invalid_json_before_starting_fresh() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"not json").unwrap();

        let store = SettingsStore::load(path.clone()).unwrap();
        assert_eq!(store.device("anything"), None);
        assert!(!path.exists());
        assert!(
            directory
                .path()
                .read_dir()
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
        );
    }

    #[test]
    fn rejects_future_schema_without_moving_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":2,"devices":{},"new_field":{"unknown":"value"}}"#,
        )
        .unwrap();

        let error = SettingsStore::load(path.clone()).err().unwrap();
        assert!(error.to_string().contains("unsupported schema version 2"));
        assert!(path.exists());
    }
}
