use std::{ffi::CString, time::Duration};

use anyhow::{Context, Result, bail};
use hidapi::{DeviceInfo, HidApi};

use crate::{ConnectionType, HidppSession, MouseDevice};

const LOGITECH_VENDOR_ID: u16 = 0x046d;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceConnection {
    DirectUsb,
    Receiver,
}

#[derive(Debug, Clone)]
pub struct ManagedDevice {
    pub id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_name: Option<String>,
    pub serial_number: Option<String>,
    pub connection: DeviceConnection,
    pub device_index: u8,
    short_path: CString,
    long_path: Option<CString>,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceChanges {
    pub connected: Vec<ManagedDevice>,
    pub disconnected: Vec<ManagedDevice>,
}

impl DeviceChanges {
    pub fn is_empty(&self) -> bool {
        self.connected.is_empty() && self.disconnected.is_empty()
    }
}

impl ManagedDevice {
    pub fn connection_type(&self) -> ConnectionType {
        match self.connection {
            DeviceConnection::DirectUsb => ConnectionType::Wired,
            DeviceConnection::Receiver => ConnectionType::GamingWireless,
        }
    }
}

/// Discovers Logitech HID++ mice and opens them without requiring VID:PID or
/// device-index arguments from callers.
pub struct DeviceManager {
    api: HidApi,
    devices: Vec<ManagedDevice>,
}

impl DeviceManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: HidApi::new()?,
            devices: Vec::new(),
        })
    }

    pub fn refresh(&mut self) -> Result<&[ManagedDevice]> {
        let mut current = self.scan_devices()?;
        reconcile_transient_identities(&self.devices, &mut current);
        self.devices = current;
        Ok(&self.devices)
    }

    /// Refreshes discovery and returns the devices whose stable IDs appeared or
    /// disappeared since the previous refresh.
    pub fn refresh_with_changes(&mut self) -> Result<DeviceChanges> {
        let previous = self.devices.clone();
        let mut current = self.scan_devices()?;
        reconcile_transient_identities(&previous, &mut current);
        let changes = diff_devices(&previous, &current);
        self.devices = current;
        Ok(changes)
    }

    fn scan_devices(&mut self) -> Result<Vec<ManagedDevice>> {
        self.api.refresh_devices()?;
        let infos: Vec<DeviceInfo> = self.api.device_list().cloned().collect();
        let mut devices = Vec::new();

        for short in infos.iter().filter(|info| {
            info.vendor_id() == LOGITECH_VENDOR_ID
                && info.usage_page() == 0xff00
                && info.usage() == 0x0001
        }) {
            let long = infos
                .iter()
                .find(|candidate| same_device(short, candidate, 0x0002));
            let connection = if is_receiver(short) {
                DeviceConnection::Receiver
            } else {
                DeviceConnection::DirectUsb
            };
            let device_index = match connection {
                DeviceConnection::DirectUsb => 0xff,
                DeviceConnection::Receiver => 0x01,
            };
            devices.push(ManagedDevice {
                id: stable_id(short, device_index),
                vendor_id: short.vendor_id(),
                product_id: short.product_id(),
                product_name: short.product_string().map(str::to_owned),
                serial_number: canonical_serial_number(short.serial_number()),
                connection,
                device_index,
                short_path: short.path().to_owned(),
                long_path: long.map(|info| info.path().to_owned()),
            });
        }

        devices.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(devices)
    }

    pub fn devices(&self) -> &[ManagedDevice] {
        &self.devices
    }

    pub fn device(&self, id: &str) -> Option<&ManagedDevice> {
        self.devices.iter().find(|device| device.id == id)
    }

    pub fn open(&self, id: &str) -> Result<MouseDevice> {
        self.open_with_timeout(id, DEFAULT_TIMEOUT)
    }

    pub fn open_with_timeout(&self, id: &str, timeout: Duration) -> Result<MouseDevice> {
        let device = self
            .device(id)
            .with_context(|| format!("managed device `{id}` was not found; refresh first"))?;
        let short = self.api.open_path(&device.short_path)?;
        let long = device
            .long_path
            .as_deref()
            .map(|path| self.api.open_path(path))
            .transpose()?;
        let session = HidppSession::new(short, long, device.device_index, timeout);
        let protocol = session.protocol_version()?;
        if protocol.major < 2 {
            bail!(
                "device `{id}` uses unsupported HID++ {}.{}",
                protocol.major,
                protocol.minor
            );
        }
        MouseDevice::discover(session)
    }
}

/// Windows can briefly enumerate the same HID interface without its USB serial while
/// a receiver is settling. Preserve the prior serial-backed routing ID when the HID
/// path still matches, or when there is exactly one unambiguous device of that type.
/// The HID++ unit ID remains the only identity used for persisted preferences.
fn reconcile_transient_identities(previous: &[ManagedDevice], current: &mut [ManagedDevice]) {
    for index in 0..current.len() {
        let device = &current[index];
        if device.serial_number.is_some() {
            continue;
        }
        let exact_path = previous.iter().find(|old| {
            old.serial_number.is_some()
                && same_managed_kind(old, device)
                && old.short_path == device.short_path
        });
        let matched = exact_path.or_else(|| {
            let previous_matches = previous
                .iter()
                .filter(|old| old.serial_number.is_some() && same_managed_kind(old, device))
                .collect::<Vec<_>>();
            let current_matches = current
                .iter()
                .filter(|candidate| same_managed_kind(candidate, device))
                .count();
            (previous_matches.len() == 1 && current_matches == 1).then_some(previous_matches[0])
        });

        if let Some(old) = matched {
            current[index].id.clone_from(&old.id);
            current[index].serial_number.clone_from(&old.serial_number);
        }
    }
}

fn same_managed_kind(left: &ManagedDevice, right: &ManagedDevice) -> bool {
    left.vendor_id == right.vendor_id
        && left.product_id == right.product_id
        && left.connection == right.connection
        && left.device_index == right.device_index
}

fn diff_devices(previous: &[ManagedDevice], current: &[ManagedDevice]) -> DeviceChanges {
    DeviceChanges {
        connected: current
            .iter()
            .filter(|device| !previous.iter().any(|old| old.id == device.id))
            .cloned()
            .collect(),
        disconnected: previous
            .iter()
            .filter(|device| !current.iter().any(|new| new.id == device.id))
            .cloned()
            .collect(),
    }
}

fn same_device(selected: &DeviceInfo, candidate: &DeviceInfo, usage: u16) -> bool {
    candidate.vendor_id() == selected.vendor_id()
        && candidate.product_id() == selected.product_id()
        && candidate.interface_number() == selected.interface_number()
        && candidate.serial_number() == selected.serial_number()
        && candidate.usage_page() == selected.usage_page()
        && candidate.usage() == usage
}

fn is_receiver(info: &DeviceInfo) -> bool {
    info.product_string()
        .is_some_and(|product| product.to_ascii_lowercase().contains("receiver"))
}

fn stable_id(info: &DeviceInfo, device_index: u8) -> String {
    let identity = canonical_serial_number(info.serial_number())
        .unwrap_or_else(|| format!("path-{:016x}", fnv1a64(info.path().to_bytes())));
    format!(
        "{:04x}:{:04x}:{identity}:{device_index:02x}",
        info.vendor_id(),
        info.product_id()
    )
}

fn canonical_serial_number(serial_number: Option<&str>) -> Option<String> {
    serial_number
        .filter(|serial| !serial.trim().is_empty())
        .map(str::to_owned)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_device(id: &str) -> ManagedDevice {
        managed_device_with(id, None, &format!("short-{id}"))
    }

    fn managed_device_with(id: &str, serial: Option<&str>, path: &str) -> ManagedDevice {
        ManagedDevice {
            id: id.to_owned(),
            vendor_id: LOGITECH_VENDOR_ID,
            product_id: 0xc094,
            product_name: Some("Test Mouse".to_owned()),
            serial_number: serial.map(str::to_owned),
            connection: DeviceConnection::DirectUsb,
            device_index: 0xff,
            short_path: CString::new(path).unwrap(),
            long_path: None,
        }
    }

    #[test]
    fn fallback_identity_hash_is_stable() {
        assert_eq!(fnv1a64(b"gflick"), fnv1a64(b"gflick"));
        assert_ne!(fnv1a64(b"gflick"), fnv1a64(b"other"));
    }

    #[test]
    fn canonicalizes_blank_usb_serials_without_changing_real_ones() {
        assert_eq!(canonical_serial_number(None), None);
        assert_eq!(canonical_serial_number(Some("")), None);
        assert_eq!(canonical_serial_number(Some(" \t\r\n")), None);
        assert_eq!(
            canonical_serial_number(Some(" REAL-SERIAL ")),
            Some(" REAL-SERIAL ".to_owned())
        );
    }

    #[test]
    fn reports_connected_and_disconnected_devices_by_stable_id() {
        let previous = [managed_device("kept"), managed_device("removed")];
        let current = [managed_device("added"), managed_device("kept")];

        let changes = diff_devices(&previous, &current);

        assert_eq!(
            changes
                .connected
                .iter()
                .map(|device| device.id.as_str())
                .collect::<Vec<_>>(),
            ["added"]
        );
        assert_eq!(
            changes
                .disconnected
                .iter()
                .map(|device| device.id.as_str())
                .collect::<Vec<_>>(),
            ["removed"]
        );
        assert!(!changes.is_empty());
        assert!(diff_devices(&current, &current).is_empty());
    }

    #[test]
    fn preserves_serial_identity_when_enumeration_temporarily_omits_it() {
        let previous = [managed_device_with(
            "046d:c54d:SERIAL:01",
            Some("SERIAL"),
            "same-path",
        )];
        let mut current = [managed_device_with(
            "046d:c54d:path-deadbeef:01",
            None,
            "same-path",
        )];

        reconcile_transient_identities(&previous, &mut current);

        assert_eq!(current[0].id, previous[0].id);
        assert_eq!(current[0].serial_number, previous[0].serial_number);
        assert!(diff_devices(&previous, &current).is_empty());
    }

    #[test]
    fn does_not_guess_between_multiple_serial_devices() {
        let previous = [
            managed_device_with("first", Some("FIRST"), "first-path"),
            managed_device_with("second", Some("SECOND"), "second-path"),
        ];
        let mut current = [managed_device_with("fallback", None, "new-path")];

        reconcile_transient_identities(&previous, &mut current);

        assert_eq!(current[0].id, "fallback");
        assert_eq!(current[0].serial_number, None);
    }
}
