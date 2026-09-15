//! Merge saved agent identities with the current USB enumeration. No client-side storage.
use gflick_protocol::DeviceSummary;
use std::collections::BTreeSet;

fn same_mouse(a: &DeviceSummary, b: &DeviceSummary) -> bool {
    match (&a.hardware_id, &b.hardware_id) {
        (Some(a), Some(b)) => a == b,
        _ => a.id == b.id,
    }
}
/// Only absence from a successful USB enumeration means disconnected.
pub fn merge(
    known: &[DeviceSummary],
    current: Vec<DeviceSummary>,
) -> (Vec<DeviceSummary>, BTreeSet<String>) {
    let mut devices = current;
    let mut disconnected = BTreeSet::new();
    for old in known {
        if devices.iter().any(|device| same_mouse(old, device)) {
            continue;
        }
        // A session ID can be reused for a different mouse. Its old identity
        // needs a separate non-routable ID in history.
        let mut old = old.clone();
        if devices.iter().any(|device| device.id == old.id) {
            old.id = format!(
                "remembered:{}",
                old.hardware_id.as_deref().unwrap_or(&old.id)
            );
        }
        old.ready = false;
        old.availability = None;
        disconnected.insert(old.id.clone());
        devices.push(old);
    }
    devices.sort_by_key(|device| device.sort_order.unwrap_or(u32::MAX));
    (devices, disconnected)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unplug_retains_identity_and_reconnect_replaces_old_usb_route() {
        let mouse = crate::test_support::device().device;
        let (known, disconnected) = merge(std::slice::from_ref(&mouse), vec![]);
        assert_eq!(known.len(), 1);
        assert!(!known[0].ready);
        assert!(disconnected.contains(&mouse.id));
        let mut returned = mouse.clone();
        returned.id = "new-usb-port".into();
        let (devices, disconnected) = merge(&known, vec![returned.clone()]);
        assert_eq!(devices, vec![returned]);
        assert!(disconnected.is_empty());
    }
    #[test]
    fn reused_usb_route_does_not_replace_a_different_physical_mouse() {
        let old = crate::test_support::device().device;
        let mut replacement = old.clone();
        replacement.hardware_id = Some("different-mouse".into());
        let (devices, disconnected) = merge(&[old], vec![replacement]);
        assert_eq!(devices.len(), 2);
        assert_ne!(devices[0].id, devices[1].id);
        assert_eq!(disconnected.len(), 1);
        let (again, _) = merge(&devices, vec![devices[0].clone()]);
        assert_eq!(again.len(), 2);
    }
    #[test]
    fn receiver_present_is_not_usb_disconnected() {
        let mouse = crate::test_support::device().device;
        let mut offline = mouse.clone();
        offline.ready = false;
        offline.availability = Some(gflick_protocol::DeviceAvailability::Unavailable {
            reason: gflick_protocol::DeviceUnavailableReason::NotResponding,
            detail: "No wireless response".into(),
        });
        let (devices, disconnected) = merge(&[mouse], vec![offline.clone()]);
        assert_eq!(devices, vec![offline]);
        assert!(disconnected.is_empty());
    }
}
