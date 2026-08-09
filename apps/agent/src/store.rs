use std::{
    collections::{BTreeMap, BTreeSet},
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
    /// Host-side display name. Never written to the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// Host-side list position; `None` sorts after every ordered device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<u32>,
    /// Model name the mouse reported over HID++, cached so a sleeping or
    /// disconnected device keeps its identity instead of falling back to the
    /// receiver's USB product name. This is the hardware's own name and stays
    /// distinct from `nickname`, so a renamed device can still show what it is.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "last_display_name"
    )]
    pub cached_model_name: Option<String>,
    /// USB identity last seen for this hardware. Lets a device that has never
    /// been opened in this run be matched to its stored name and nickname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usb_identity: Option<UsbIdentity>,
}

/// Pre-HID++ identity of a device. A receiver's vendor/product pair is shared by
/// every mouse paired to it, so `device_index` is part of the match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_index: u8,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_serial_number"
    )]
    pub serial_number: Option<String>,
}

impl UsbIdentity {
    pub fn new(
        vendor_id: u16,
        product_id: u16,
        device_index: u8,
        serial_number: Option<String>,
    ) -> Self {
        Self {
            vendor_id,
            product_id,
            device_index,
            serial_number: canonical_serial_number(serial_number),
        }
    }

    fn canonicalized(&self) -> Self {
        Self::new(
            self.vendor_id,
            self.product_id,
            self.device_index,
            self.serial_number.clone(),
        )
    }

    fn same_usb_slot(&self, other: &Self) -> bool {
        self.vendor_id == other.vendor_id
            && self.product_id == other.product_id
            && self.device_index == other.device_index
    }
}

fn canonical_serial_number(serial_number: Option<String>) -> Option<String> {
    serial_number.filter(|serial| !serial.trim().is_empty())
}

fn deserialize_serial_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(canonical_serial_number)
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

    /// Replaces the complete user-defined device order. Every requested hardware
    /// ID must already have stored preferences, and omitted devices become
    /// unordered.
    pub fn replace_device_order(&mut self, hardware_ids: &[String]) -> Result<()> {
        let mut seen = BTreeSet::new();
        for (position, hardware_id) in hardware_ids.iter().enumerate() {
            if !seen.insert(hardware_id) {
                bail!("duplicate hardware ID in reorder request");
            }
            if !self.document.devices.contains_key(hardware_id) {
                bail!("unknown hardware ID in reorder request");
            }
            u32::try_from(position).context("device order is out of range")?;
        }

        for preferences in self.document.devices.values_mut() {
            preferences.sort_order = None;
        }
        for (position, hardware_id) in hardware_ids.iter().enumerate() {
            let order = u32::try_from(position).context("device order is out of range")?;
            self.document
                .devices
                .get_mut(hardware_id)
                .expect("reorder validation confirmed the hardware ID exists")
                .sort_order = Some(order);
        }
        Ok(())
    }

    /// Resolves stored preferences for a device that has not been opened yet by
    /// the USB identity recorded the last time it was ready. Exact identities win.
    /// A missing serial may fall back to the same USB slot only when that match is
    /// unique; ambiguity fails closed.
    pub fn resolve_device_by_usb_identity(
        &self,
        identity: &UsbIdentity,
    ) -> Option<(&str, &DevicePreferences)> {
        let identity = identity.canonicalized();
        let stored_identities = self
            .document
            .devices
            .iter()
            .filter_map(|(hardware_id, preferences)| {
                preferences
                    .usb_identity
                    .as_ref()
                    .map(UsbIdentity::canonicalized)
                    .map(|stored| (hardware_id.as_str(), preferences, stored))
            })
            .collect::<Vec<_>>();

        let exact_matches = stored_identities
            .iter()
            .filter(|(_, _, stored)| stored == &identity)
            .map(|(hardware_id, preferences, _)| (*hardware_id, *preferences))
            .collect::<Vec<_>>();
        match exact_matches.as_slice() {
            [matched] => return Some(*matched),
            [] => {}
            _ => return None,
        }

        let relaxed_matches = stored_identities
            .iter()
            .filter(|(_, _, stored)| {
                stored.same_usb_slot(&identity)
                    && (stored.serial_number.is_none() || identity.serial_number.is_none())
            })
            .map(|(hardware_id, preferences, _)| (*hardware_id, *preferences))
            .collect::<Vec<_>>();
        match relaxed_matches.as_slice() {
            [matched] => Some(*matched),
            _ => None,
        }
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

    fn usb_identity(device_index: u8, serial_number: Option<&str>) -> UsbIdentity {
        UsbIdentity::new(
            0x046d,
            0xc54d,
            device_index,
            serial_number.map(str::to_owned),
        )
    }

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
            nickname: None,
            sort_order: None,
            cached_model_name: None,
            usb_identity: None,
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
    fn persists_host_side_nickname_and_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path.clone()).unwrap();
        let device = store.device_mut("046d:unit:1077e69f");
        device.nickname = Some("Desk left".to_owned());
        device.sort_order = Some(2);
        store.save().unwrap();

        let loaded = SettingsStore::load(path).unwrap();
        let device = loaded.device("046d:unit:1077e69f").unwrap();
        assert_eq!(device.nickname.as_deref(), Some("Desk left"));
        assert_eq!(device.sort_order, Some(2));
    }

    #[test]
    fn replaces_device_order_and_persists_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path.clone()).unwrap();
        store.device_mut("hardware-a");
        store.device_mut("hardware-b");

        store
            .replace_device_order(&["hardware-a".to_owned(), "hardware-b".to_owned()])
            .unwrap();
        assert_eq!(store.device("hardware-a").unwrap().sort_order, Some(0));
        assert_eq!(store.device("hardware-b").unwrap().sort_order, Some(1));
        store.save().unwrap();

        let loaded = SettingsStore::load(path).unwrap();
        assert_eq!(loaded.device("hardware-a").unwrap().sort_order, Some(0));
        assert_eq!(loaded.device("hardware-b").unwrap().sort_order, Some(1));
    }

    #[test]
    fn replacement_order_clears_omitted_devices() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path).unwrap();
        store.device_mut("hardware-a");
        store.device_mut("hardware-b");

        store
            .replace_device_order(&["hardware-a".to_owned(), "hardware-b".to_owned()])
            .unwrap();
        store
            .replace_device_order(&["hardware-b".to_owned()])
            .unwrap();

        assert_eq!(store.device("hardware-a").unwrap().sort_order, None);
        assert_eq!(store.device("hardware-b").unwrap().sort_order, Some(0));
    }

    #[test]
    fn empty_replacement_order_clears_all_devices() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path).unwrap();
        store.device_mut("hardware-a").sort_order = Some(0);
        store.device_mut("hardware-b").sort_order = Some(1);

        store.replace_device_order(&[]).unwrap();

        assert_eq!(store.device("hardware-a").unwrap().sort_order, None);
        assert_eq!(store.device("hardware-b").unwrap().sort_order, None);
    }

    #[test]
    fn duplicate_replacement_hardware_id_leaves_preferences_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path).unwrap();
        store.device_mut("hardware-a").sort_order = Some(1);
        store.device_mut("hardware-b").sort_order = Some(0);

        let error = store
            .replace_device_order(&["hardware-a".to_owned(), "hardware-a".to_owned()])
            .unwrap_err();

        assert!(error.to_string().contains("duplicate"));
        assert_eq!(store.device("hardware-a").unwrap().sort_order, Some(1));
        assert_eq!(store.device("hardware-b").unwrap().sort_order, Some(0));
    }

    #[test]
    fn unknown_replacement_hardware_id_creates_no_record_or_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path).unwrap();
        store.device_mut("hardware-a").sort_order = Some(0);
        store.device_mut("hardware-b").sort_order = Some(1);

        let error = store
            .replace_device_order(&["hardware-a".to_owned(), "missing".to_owned()])
            .unwrap_err();

        assert!(error.to_string().contains("unknown hardware ID"));
        assert_eq!(store.device("hardware-a").unwrap().sort_order, Some(0));
        assert_eq!(store.device("hardware-b").unwrap().sort_order, Some(1));
        assert_eq!(store.device("missing"), None);
    }

    #[test]
    fn persists_the_cached_model_name() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut store = SettingsStore::load(path.clone()).unwrap();
        store.device_mut("046d:unit:1077e69f").cached_model_name =
            Some("PRO X Superlight 2".to_owned());
        store.save().unwrap();

        let loaded = SettingsStore::load(path).unwrap();
        assert_eq!(
            loaded
                .device("046d:unit:1077e69f")
                .unwrap()
                .cached_model_name
                .as_deref(),
            Some("PRO X Superlight 2")
        );
    }

    /// Files written before the rename must keep their cached name.
    #[test]
    fn reads_the_pre_rename_display_name_field() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"devices":{"unit":{"last_display_name":"PRO X Wireless"}}}"#,
        )
        .unwrap();

        let store = SettingsStore::load(path).unwrap();
        assert_eq!(
            store.device("unit").unwrap().cached_model_name.as_deref(),
            Some("PRO X Wireless")
        );
    }

    #[test]
    fn finds_a_never_opened_device_by_its_usb_identity() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(directory.path().join("settings.json")).unwrap();
        let identity = usb_identity(1, None);
        let device = store.device_mut("046d:unit:1077e69f");
        device.cached_model_name = Some("PRO X Superlight 2".to_owned());
        device.nickname = Some("Desk mouse".to_owned());
        device.usb_identity = Some(identity.clone());

        let (hardware_id, preferences) = store.resolve_device_by_usb_identity(&identity).unwrap();
        assert_eq!(hardware_id, "046d:unit:1077e69f");
        assert_eq!(preferences.nickname.as_deref(), Some("Desk mouse"));
    }

    #[test]
    fn exact_serial_match_wins() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(directory.path().join("settings.json")).unwrap();
        let identity = usb_identity(1, Some("SERIAL-A"));
        store.device_mut("unit-a").usb_identity = Some(identity.clone());
        store.device_mut("unit-without-serial").usb_identity = Some(usb_identity(1, None));

        assert_eq!(
            store.resolve_device_by_usb_identity(&identity).unwrap().0,
            "unit-a"
        );
    }

    #[test]
    fn canonicalizes_blank_serials_as_missing() {
        assert_eq!(usb_identity(1, None).serial_number, None);
        assert_eq!(usb_identity(1, Some("")).serial_number, None);
        assert_eq!(usb_identity(1, Some(" \t\r\n")).serial_number, None);
        assert_eq!(
            usb_identity(1, Some(" SERIAL ")).serial_number.as_deref(),
            Some(" SERIAL ")
        );
    }

    #[test]
    fn uniquely_relaxes_a_match_when_either_serial_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let mut stored_without_serial =
            SettingsStore::load(directory.path().join("missing-stored.json")).unwrap();
        stored_without_serial.device_mut("unit-a").usb_identity = Some(usb_identity(1, None));
        assert_eq!(
            stored_without_serial
                .resolve_device_by_usb_identity(&usb_identity(1, Some("SERIAL-A")))
                .unwrap()
                .0,
            "unit-a"
        );

        let mut candidate_without_serial =
            SettingsStore::load(directory.path().join("missing-candidate.json")).unwrap();
        candidate_without_serial.device_mut("unit-b").usb_identity =
            Some(usb_identity(1, Some("SERIAL-B")));
        assert_eq!(
            candidate_without_serial
                .resolve_device_by_usb_identity(&usb_identity(1, None))
                .unwrap()
                .0,
            "unit-b"
        );
    }

    #[test]
    fn ambiguous_relaxed_match_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(directory.path().join("settings.json")).unwrap();
        store.device_mut("unit-a").usb_identity = Some(usb_identity(1, Some("SERIAL-A")));
        store.device_mut("unit-b").usb_identity = Some(usb_identity(1, Some("SERIAL-B")));

        assert!(
            store
                .resolve_device_by_usb_identity(&usb_identity(1, None))
                .is_none()
        );
    }

    #[test]
    fn ambiguous_exact_match_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(directory.path().join("settings.json")).unwrap();
        let identity = usb_identity(1, Some("SERIAL-A"));
        store.device_mut("unit-a").usb_identity = Some(identity.clone());
        store.device_mut("unit-b").usb_identity = Some(identity.clone());

        assert!(store.resolve_device_by_usb_identity(&identity).is_none());
    }

    #[test]
    fn distinct_real_serials_never_relax_match() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(directory.path().join("settings.json")).unwrap();
        store.device_mut("unit-a").usb_identity = Some(usb_identity(1, Some("SERIAL-A")));

        assert!(
            store
                .resolve_device_by_usb_identity(&usb_identity(1, Some("SERIAL-B")))
                .is_none()
        );
    }

    #[test]
    fn historical_empty_serial_loads_and_matches_a_missing_serial() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"devices":{"unit-a":{"usb_identity":{"vendor_id":1133,"product_id":50509,"device_index":1,"serial_number":""}}}}"#,
        )
        .unwrap();

        let store = SettingsStore::load(path.clone()).unwrap();
        assert_eq!(
            store
                .device("unit-a")
                .unwrap()
                .usb_identity
                .as_ref()
                .unwrap()
                .serial_number,
            None
        );
        assert_eq!(
            store
                .resolve_device_by_usb_identity(&usb_identity(1, None))
                .unwrap()
                .0,
            "unit-a"
        );
        store.save().unwrap();
        assert!(!fs::read_to_string(path).unwrap().contains("serial_number"));
    }

    /// One receiver serves several mice, so the paired slot must be part of the match.
    #[test]
    fn usb_identity_match_distinguishes_devices_behind_one_receiver() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(directory.path().join("settings.json")).unwrap();
        let first = usb_identity(1, None);
        let second = usb_identity(2, None);
        store.device_mut("unit-a").usb_identity = Some(first.clone());
        store.device_mut("unit-b").usb_identity = Some(second.clone());

        assert_eq!(
            store.resolve_device_by_usb_identity(&first).unwrap().0,
            "unit-a"
        );
        assert_eq!(
            store.resolve_device_by_usb_identity(&second).unwrap().0,
            "unit-b"
        );
    }

    /// Settings written before these fields existed must still load.
    #[test]
    fn loads_preferences_without_host_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"devices":{"unit":{"host":{"dpi":800}}}}"#,
        )
        .unwrap();

        let store = SettingsStore::load(path).unwrap();
        let device = store.device("unit").unwrap();
        assert_eq!(device.nickname, None);
        assert_eq!(device.sort_order, None);
        assert_eq!(device.cached_model_name, None);
        assert_eq!(device.usb_identity, None);
        assert_eq!(device.host.dpi, Some(800));
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
