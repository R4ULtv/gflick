use std::borrow::Cow;

use anyhow::{Result, bail};
use gflick_protocol::{DeviceAvailability, DeviceSummary};

pub fn select_device(devices: &[DeviceSummary], selector: Option<&str>) -> Result<DeviceSummary> {
    match selector {
        None => match devices {
            [] => bail!("no devices are available from the background agent"),
            [device] => Ok(device.clone()),
            _ => bail!(
                "multiple devices are available; specify a selector:\n{}",
                candidates(devices)
            ),
        },
        Some(selector) => select_explicit(devices, selector),
    }
}

fn select_explicit(devices: &[DeviceSummary], selector: &str) -> Result<DeviceSummary> {
    if let Some(device) = devices.iter().find(|device| device.id == selector) {
        return Ok(device.clone());
    }
    if let Some(device) = devices
        .iter()
        .find(|device| device.hardware_id.as_deref() == Some(selector))
    {
        return Ok(device.clone());
    }

    let selector = selector.to_ascii_lowercase();
    let matches = devices
        .iter()
        .filter(|device| {
            device.id.to_ascii_lowercase().starts_with(&selector)
                || device
                    .hardware_id
                    .as_deref()
                    .is_some_and(|id| id.to_ascii_lowercase().starts_with(&selector))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [device] => Ok((*device).clone()),
        [] => bail!(
            "no device matches `{selector}`; available devices:\n{}",
            candidates(devices)
        ),
        _ => bail!(
            "selector `{selector}` is ambiguous; available devices:\n{}",
            candidates(devices)
        ),
    }
}

pub fn device_name(device: &DeviceSummary) -> Cow<'_, str> {
    if let Some(name) = device
        .display_name
        .as_deref()
        .or(device.product_name.as_deref())
    {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(format!(
            "{:04x}:{:04x}",
            device.vendor_id, device.product_id
        ))
    }
}

pub fn stable_selector(device: &DeviceSummary) -> &str {
    device.hardware_id.as_deref().unwrap_or(&device.id)
}

pub fn device_status(device: &DeviceSummary) -> Cow<'_, str> {
    match &device.availability {
        Some(DeviceAvailability::Initializing) => Cow::Borrowed("initializing"),
        Some(DeviceAvailability::Ready) => Cow::Borrowed("ready"),
        Some(DeviceAvailability::Unavailable { detail, .. }) => {
            Cow::Owned(format!("unavailable: {detail}"))
        }
        None if device.ready => Cow::Borrowed("ready"),
        None => Cow::Borrowed("initializing"),
    }
}

fn candidates(devices: &[DeviceSummary]) -> String {
    devices
        .iter()
        .map(|device| format!("  {} ({})", device_name(device), stable_selector(device)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use gflick_protocol::DeviceConnection;

    use super::*;

    fn device(id: &str, hardware_id: Option<&str>) -> DeviceSummary {
        DeviceSummary {
            id: id.to_owned(),
            hardware_id: hardware_id.map(str::to_owned),
            vendor_id: 0x046d,
            product_id: 0xc539,
            product_name: Some("Receiver".to_owned()),
            display_name: None,
            serial_number: None,
            nickname: None,
            color: Default::default(),
            sort_order: None,
            connection: DeviceConnection::Receiver,
            device_index: 1,
            availability: None,
            ready: true,
        }
    }

    #[test]
    fn zero_devices_fails() {
        assert!(
            select_device(&[], None)
                .unwrap_err()
                .to_string()
                .contains("no devices")
        );
    }

    #[test]
    fn implicit_single_device_uses_session_id() {
        assert_eq!(
            select_device(&[device("session", Some("hardware"))], None)
                .unwrap()
                .id,
            "session"
        );
    }

    #[test]
    fn implicit_multiple_devices_fails_with_candidates() {
        let error = select_device(
            &[
                device("session-a", Some("hardware-a")),
                device("session-b", Some("hardware-b")),
            ],
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("hardware-a"));
    }

    #[test]
    fn exact_session_id_routes_to_session() {
        assert_eq!(
            select_device(&[device("Session", Some("hardware"))], Some("Session"))
                .unwrap()
                .id,
            "Session"
        );
    }

    #[test]
    fn exact_hardware_id_routes_to_session() {
        assert_eq!(
            select_device(&[device("session", Some("hardware"))], Some("hardware"))
                .unwrap()
                .id,
            "session"
        );
    }

    #[test]
    fn unique_case_insensitive_prefix_matches() {
        assert_eq!(
            select_device(&[device("session", Some("hardware"))], Some("HAR"))
                .unwrap()
                .id,
            "session"
        );
    }

    #[test]
    fn ambiguous_prefix_fails() {
        assert!(
            select_device(
                &[
                    device("session-a", Some("hardware-a")),
                    device("session-b", Some("hardware-b"))
                ],
                Some("session")
            )
            .is_err()
        );
    }

    #[test]
    fn unavailable_listing_is_still_selectable() {
        let mut unavailable = device("session", None);
        unavailable.ready = false;
        assert_eq!(select_device(&[unavailable], None).unwrap().id, "session");
    }

    #[test]
    fn device_name_prioritizes_display_product_then_vid_pid() {
        let mut item = device("session", None);
        item.display_name = Some("G305".to_owned());
        assert_eq!(device_name(&item), "G305");
        item.display_name = None;
        assert_eq!(device_name(&item), "Receiver");
        item.product_name = None;
        assert_eq!(device_name(&item), "046d:c539");
    }
}
