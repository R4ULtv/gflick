//! Agent events update telemetry without refreshing editable hardware settings.
use crate::agent::{self, Snapshot};
use gflick_protocol::{
    AgentEvent, DeviceAvailability, DeviceState, DeviceSummary, DeviceUnavailableReason,
};
use std::collections::BTreeSet;

pub enum Update {
    Snapshot(Snapshot),
    Event(AgentEvent),
    Offline(String),
}

#[derive(Default)]
struct ListenerState {
    closed: bool,
    cancellation: Option<gflick_client::SubscriptionCancellation>,
}
/// Owned by the GPUI task, so closing the window cancels even an idle stream.
pub struct Listener(std::sync::Arc<std::sync::Mutex<ListenerState>>);
impl Drop for Listener {
    fn drop(&mut self) {
        let cancellation = {
            let mut state = self.0.lock().expect("settings listener mutex poisoned");
            state.closed = true;
            state.cancellation.take()
        };
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
        }
    }
}

/// A dedicated blocking IPC thread, like the tray. A bounded channel wakes GPUI
/// only when something changes; no UI polling or extra battery HID reads.
pub fn subscribe() -> (async_channel::Receiver<Update>, Listener) {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ListenerState::default()));
    let listener = Listener(state.clone());
    let (sender, receiver) = async_channel::bounded(128);
    std::thread::Builder::new()
        .name("settings-agent-events".into())
        .spawn(move || {
            while !sender.is_closed() {
                let result = (|| -> anyhow::Result<()> {
                    let mut subscription = gflick_client::EventSubscription::connect_settings()?;
                    let cancellation = subscription.cancellation_handle()?;
                    {
                        let mut state = state.lock().expect("settings listener mutex poisoned");
                        if state.closed {
                            cancellation.cancel();
                            return Ok(());
                        }
                        state.cancellation = Some(cancellation);
                    }
                    // Subscribe before taking the baseline so transitions during the
                    // read remain queued on the socket and are replayed afterwards.
                    sender.send_blocking(Update::Snapshot(agent::load_snapshot()?))?;
                    while let Some(event) = subscription.recv()? {
                        sender.send_blocking(Update::Event(event))?;
                        if sender.is_closed() {
                            return Ok(());
                        }
                    }
                    anyhow::bail!("The agent closed the event connection")
                })();
                if let Err(error) = result
                    && sender
                        .send_blocking(Update::Offline(format!("{error:#}")))
                        .is_err()
                {
                    return;
                }
                let cancellation = state
                    .lock()
                    .expect("settings listener mutex poisoned")
                    .cancellation
                    .take();
                if let Some(cancellation) = cancellation {
                    cancellation.cancel();
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        })
        .expect("could not start settings event worker");
    (receiver, listener)
}

fn upsert(
    devices: &mut Vec<DeviceSummary>,
    states: &mut Vec<DeviceState>,
    disconnected: &mut BTreeSet<String>,
    device: DeviceSummary,
) {
    // A receiver/USB path may be reused for another physical mouse. Preserve
    // the old mouse as a disconnected saved row rather than replacing it.
    if let Some(old) = devices.iter_mut().find(|old| {
        old.id == device.id
            && old.hardware_id.is_some()
            && device.hardware_id.is_some()
            && old.hardware_id != device.hardware_id
    }) {
        states.retain(|state| state.device.id != old.id);
        disconnected.remove(&old.id);
        old.id = format!("saved:{}", old.hardware_id.as_deref().unwrap());
        old.ready = false;
        old.availability = None;
        disconnected.insert(old.id.clone());
    }
    let existing = devices.iter().position(|old| {
        old.id == device.id
            || (device.hardware_id.is_some() && old.hardware_id == device.hardware_id)
    });
    disconnected.remove(&device.id);
    if let Some(index) = existing {
        let old_id = devices[index].id.clone();
        disconnected.remove(&old_id);
        if old_id != device.id {
            states.retain(|state| state.device.id != old_id);
        }
        devices[index] = device;
    } else {
        devices.push(device);
    }
    devices.sort_by_key(|d| d.sort_order.unwrap_or(u32::MAX));
}

pub fn apply(
    devices: &mut Vec<DeviceSummary>,
    states: &mut Vec<DeviceState>,
    disconnected: &mut BTreeSet<String>,
    event: AgentEvent,
) {
    match event {
        AgentEvent::BatteryChanged { device_id, battery } => {
            // Late battery events must never revive an unavailable device.
            if devices.iter().any(|d| d.id == device_id && d.ready)
                && !disconnected.contains(&device_id)
                && let Some(state) = states.iter_mut().find(|s| s.device.id == device_id)
            {
                state.settings.battery = battery;
            }
        }
        AgentEvent::DeviceConnected { device } => {
            states.retain(|s| s.device.id != device.id);
            upsert(devices, states, disconnected, device);
        }
        AgentEvent::DeviceReady { device } => {
            upsert(devices, states, disconnected, device.device.clone());
            if let Some(state) = states.iter_mut().find(|s| s.device.id == device.device.id) {
                state.device = device.device;
                state.settings.battery = device.settings.battery;
            } else {
                // A new/reconnected mouse needs an initial settings baseline.
                states.push(*device);
            }
        }
        AgentEvent::DeviceUnavailable {
            device_id,
            reason_code,
            reason,
        } => {
            if let Some(device) = devices.iter_mut().find(|d| d.id == device_id) {
                device.ready = false;
                device.availability = Some(DeviceAvailability::Unavailable {
                    reason: reason_code.unwrap_or(DeviceUnavailableReason::NotResponding),
                    detail: reason,
                });
                disconnected.remove(&device_id);
            }
            states.retain(|s| s.device.id != device_id);
        }
        AgentEvent::DeviceDisconnected { device_id } => {
            if let Some(device) = devices.iter_mut().find(|d| d.id == device_id) {
                device.ready = false;
                device.availability = None;
                disconnected.insert(device_id.clone());
            }
            states.retain(|s| s.device.id != device_id);
        }
        // Hardware and presentation changes remain manual. This also avoids
        // applying somebody else's settings over an in-progress local draft.
        AgentEvent::SettingsChanged { .. }
        | AgentEvent::DeviceMetadataChanged { .. }
        | AgentEvent::ApplicationShuttingDown => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gflick_protocol::BatteryState;

    fn battery(percentage: u8, status_code: u8) -> Option<BatteryState> {
        Some(BatteryState {
            percentage,
            level_code: 0,
            status_code,
            status: if status_code == 1 {
                "charging"
            } else {
                "discharging"
            }
            .into(),
        })
    }
    #[test]
    fn battery_updates_charge_status_without_refreshing_other_settings() {
        let state = crate::test_support::device();
        let mut devices = vec![state.device.clone()];
        let mut states = vec![state.clone()];
        let mut disconnected = BTreeSet::new();
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::BatteryChanged {
                device_id: state.device.id.clone(),
                battery: battery(74, 1),
            },
        );
        let mut expected = state;
        expected.settings.battery = battery(74, 1);
        assert_eq!(states, vec![expected.clone()]);
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::BatteryChanged {
                device_id: expected.device.id,
                battery: None,
            },
        );
        assert!(states[0].settings.battery.is_none());
    }
    #[test]
    fn hardware_and_metadata_events_stay_manual() {
        let state = crate::test_support::device();
        let mut devices = vec![state.device.clone()];
        let mut states = vec![state.clone()];
        let mut changed = state.clone();
        changed.settings.dpi = None;
        changed.device.nickname = Some("External name".into());
        apply(
            &mut devices,
            &mut states,
            &mut BTreeSet::new(),
            AgentEvent::SettingsChanged {
                device: Box::new(changed.clone()),
            },
        );
        apply(
            &mut devices,
            &mut states,
            &mut BTreeSet::new(),
            AgentEvent::DeviceMetadataChanged {
                devices: vec![changed.device],
            },
        );
        assert_eq!(devices, vec![state.device.clone()]);
        assert_eq!(states, vec![state]);
    }
    #[test]
    fn offline_and_usb_disconnected_are_distinct_and_clear_battery() {
        let state = crate::test_support::device();
        let id = state.device.id.clone();
        let mut devices = vec![state.device.clone()];
        let mut states = vec![state];
        let mut disconnected = BTreeSet::new();
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::DeviceUnavailable {
                device_id: id.clone(),
                reason_code: Some(DeviceUnavailableReason::NotResponding),
                reason: "Mouse asleep".into(),
            },
        );
        assert!(!devices[0].ready);
        assert!(states.is_empty());
        assert!(disconnected.is_empty());
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::DeviceDisconnected {
                device_id: id.clone(),
            },
        );
        assert!(disconnected.contains(&id));
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::BatteryChanged {
                device_id: id,
                battery: battery(100, 1),
            },
        );
        assert!(states.is_empty());
        assert!(!devices[0].ready);
    }
    #[test]
    fn reused_receiver_route_preserves_the_previous_mouse() {
        let state = crate::test_support::device();
        let mut devices = vec![state.device.clone()];
        let mut states = vec![state.clone()];
        let mut disconnected = BTreeSet::new();
        let mut replacement = state;
        replacement.device.hardware_id = Some("other-mouse".into());
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::DeviceReady {
                device: Box::new(replacement.clone()),
            },
        );
        assert_eq!(devices.len(), 2);
        assert_eq!(disconnected.len(), 1);
        assert_eq!(states, vec![replacement]);
        assert_ne!(devices[0].id, devices[1].id);
    }
    #[test]
    fn reconnect_on_a_new_usb_route_replaces_the_remembered_row() {
        let state = crate::test_support::device();
        let mut devices = vec![state.device.clone()];
        let mut states = vec![state.clone()];
        let mut disconnected = BTreeSet::new();
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::DeviceDisconnected {
                device_id: state.device.id.clone(),
            },
        );
        let mut returned = state;
        returned.device.id = "another-usb-port".into();
        apply(
            &mut devices,
            &mut states,
            &mut disconnected,
            AgentEvent::DeviceReady {
                device: Box::new(returned.clone()),
            },
        );
        assert_eq!(devices, vec![returned.device.clone()]);
        assert_eq!(states, vec![returned]);
        assert!(disconnected.is_empty());
    }
}
