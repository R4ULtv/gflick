use gflick_protocol::{
    AgentEvent, BatteryState, DeviceState, DeviceSummary, DpiState, SettingsState,
};

#[derive(Debug, Default)]
pub struct TrayState {
    pub agent_connected: bool,
    devices: Vec<DeviceView>,
}

#[derive(Debug, Clone)]
struct DeviceView {
    summary: DeviceSummary,
    settings: Option<SettingsState>,
    unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStatus {
    pub name: String,
    pub status: Option<String>,
    pub battery: String,
    pub dpi: String,
}

impl TrayState {
    pub fn replace(&mut self, devices: Vec<(DeviceSummary, Option<DeviceState>)>) {
        self.agent_connected = true;
        self.devices = devices
            .into_iter()
            .map(|(summary, state)| {
                state.map_or_else(
                    || DeviceView {
                        unavailable_reason: availability_reason(&summary),
                        summary,
                        settings: None,
                    },
                    DeviceView::from_state,
                )
            })
            .collect();
    }

    pub fn disconnect_agent(&mut self) {
        self.agent_connected = false;
        self.devices.clear();
    }

    pub fn apply(&mut self, event: AgentEvent) {
        self.agent_connected = true;
        match event {
            AgentEvent::ApplicationShuttingDown => self.disconnect_agent(),
            AgentEvent::DeviceConnected { device } => {
                self.replace_or_append(DeviceView {
                    unavailable_reason: availability_reason(&device),
                    summary: device,
                    settings: None,
                });
            }
            AgentEvent::DeviceReady { device } | AgentEvent::SettingsChanged { device } => {
                self.replace_or_append(DeviceView::from_state(*device));
            }
            AgentEvent::DeviceMetadataChanged { devices } => self.merge_metadata(devices),
            AgentEvent::DeviceUnavailable {
                device_id, reason, ..
            } => {
                if let Some(device) = self.device_mut(&device_id) {
                    device.settings = None;
                    device.unavailable_reason = Some(reason);
                }
            }
            AgentEvent::DeviceDisconnected { device_id } => {
                if let Some(index) = self.device_index(&device_id) {
                    self.devices.remove(index);
                }
            }
            AgentEvent::BatteryChanged { device_id, battery } => {
                if let Some(settings) = self
                    .device_mut(&device_id)
                    .and_then(|device| device.settings.as_mut())
                {
                    settings.battery = battery;
                }
            }
        }
    }

    pub fn statuses(&self) -> Vec<DeviceStatus> {
        self.devices.iter().map(DeviceView::status).collect()
    }

    pub fn tooltip(&self) -> String {
        if !self.agent_connected {
            return "gflick — agent offline".to_owned();
        }
        let Some(device) = self.devices.iter().find(|device| device.settings.is_some()) else {
            return if self.devices.is_empty() {
                "gflick — no mouse connected".to_owned()
            } else {
                "gflick — mouse unavailable".to_owned()
            };
        };
        let status = device.status();
        truncate(
            &format!(
                "gflick — {}\n{} · {}",
                status.name, status.battery, status.dpi
            ),
            120,
        )
    }

    pub fn title(&self) -> String {
        if !self.agent_connected {
            return "Offline".to_owned();
        }
        let Some(device) = self.devices.iter().find(|device| device.settings.is_some()) else {
            return if self.devices.is_empty() {
                "No mouse".to_owned()
            } else {
                "Unavailable".to_owned()
            };
        };
        let settings = device.settings.as_ref().expect("settings checked above");
        let battery = settings.battery.as_ref().map_or_else(
            || "Battery —".to_owned(),
            |battery| format!("{}%", battery.percentage),
        );
        let dpi = settings.dpi.as_ref().map_or_else(
            || "DPI —".to_owned(),
            |dpi| format!("{} DPI", dpi.current_x),
        );
        format!("{battery} · {dpi}")
    }

    fn device_index(&self, device_id: &str) -> Option<usize> {
        self.devices
            .iter()
            .position(|device| device.summary.id == device_id)
    }

    fn device_mut(&mut self, device_id: &str) -> Option<&mut DeviceView> {
        self.devices
            .iter_mut()
            .find(|device| device.summary.id == device_id)
    }

    fn replace_or_append(&mut self, device: DeviceView) {
        if let Some(index) = self.device_index(&device.summary.id) {
            self.devices[index] = device;
        } else {
            self.devices.push(device);
        }
    }

    /// Replaces the authoritative ordered summaries while retaining any live
    /// session state that metadata events intentionally do not contain.
    fn merge_metadata(&mut self, summaries: Vec<DeviceSummary>) {
        let mut existing = std::mem::take(&mut self.devices);
        self.devices = summaries
            .into_iter()
            .map(|summary| {
                if let Some(index) = existing
                    .iter()
                    .position(|device| device.summary.id == summary.id)
                {
                    let device = existing.remove(index);
                    DeviceView {
                        summary,
                        settings: device.settings,
                        unavailable_reason: device.unavailable_reason,
                    }
                } else {
                    DeviceView {
                        unavailable_reason: availability_reason(&summary),
                        summary,
                        settings: None,
                    }
                }
            })
            .collect();
    }
}

impl DeviceView {
    fn from_state(state: DeviceState) -> Self {
        Self {
            summary: state.device,
            settings: Some(state.settings),
            unavailable_reason: None,
        }
    }

    fn status(&self) -> DeviceStatus {
        let name = device_label(&self.summary);
        let Some(settings) = self.settings.as_ref() else {
            return DeviceStatus {
                name,
                status: Some(
                    self.unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "Connecting…".to_owned()),
                ),
                battery: "Battery: unavailable".to_owned(),
                dpi: "DPI: unavailable".to_owned(),
            };
        };
        DeviceStatus {
            name,
            status: None,
            battery: battery_label(settings.battery.as_ref()),
            dpi: dpi_label(settings.dpi.as_ref()),
        }
    }
}

/// User nickname wins over the reported model name, matching the settings UI.
/// A blank stored value falls through so the entry is never nameless.
fn device_label(summary: &DeviceSummary) -> String {
    summary
        .nickname
        .as_deref()
        .map(str::trim)
        .filter(|nickname| !nickname.is_empty())
        .map(str::to_owned)
        .or_else(|| summary.display_name.clone())
        .or_else(|| summary.product_name.clone())
        .unwrap_or_else(|| format!("Mouse {:04x}:{:04x}", summary.vendor_id, summary.product_id))
}

fn battery_label(battery: Option<&BatteryState>) -> String {
    battery.map_or_else(
        || "Battery: unavailable".to_owned(),
        |battery| format!("Battery: {}% ({})", battery.percentage, battery.status),
    )
}

fn dpi_label(dpi: Option<&DpiState>) -> String {
    dpi.map_or_else(
        || "DPI: unavailable".to_owned(),
        |dpi| match dpi.current_y {
            Some(y) if y != dpi.current_x => format!("DPI: {} × {y}", dpi.current_x),
            _ => format!("DPI: {}", dpi.current_x),
        },
    )
}

fn availability_reason(summary: &DeviceSummary) -> Option<String> {
    match summary.availability.as_ref() {
        Some(gflick_protocol::DeviceAvailability::Unavailable { detail, .. }) => {
            Some(detail.clone())
        }
        _ if summary.ready => None,
        _ => Some("Connecting…".to_owned()),
    }
}

fn truncate(value: &str, maximum_characters: usize) -> String {
    if value.chars().count() <= maximum_characters {
        return value.to_owned();
    }
    value
        .chars()
        .take(maximum_characters.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gflick_protocol::{
        DeviceAvailability, DeviceCapabilities, DeviceConnection, PollingRateCapabilities,
        PollingRateState,
    };

    #[test]
    fn displays_ready_mouse_battery_and_dpi() {
        let state = sample_state();
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);
        assert_eq!(tray.title(), "84% · 800 DPI");
        assert!(tray.tooltip().contains("Battery: 84%"));
        assert_eq!(tray.statuses()[0].dpi, "DPI: 800");
    }

    #[test]
    fn battery_events_update_without_replacing_device_state() {
        let state = sample_state();
        let id = state.device.id.clone();
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);
        tray.apply(AgentEvent::BatteryChanged {
            device_id: id,
            battery: Some(BatteryState {
                percentage: 83,
                level_code: 8,
                status_code: 0,
                status: "discharging".to_owned(),
            }),
        });
        assert_eq!(tray.title(), "83% · 800 DPI");
    }

    #[test]
    fn disconnected_agent_never_displays_stale_values() {
        let state = sample_state();
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);
        tray.disconnect_agent();
        assert_eq!(tray.title(), "Offline");
        assert!(tray.statuses().is_empty());
    }

    #[test]
    fn unavailable_mouse_never_displays_stale_values() {
        let state = sample_state();
        let id = state.device.id.clone();
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);
        tray.apply(AgentEvent::DeviceUnavailable {
            device_id: id,
            reason_code: Some(gflick_protocol::DeviceUnavailableReason::NotResponding),
            reason: "wireless link disconnected".to_owned(),
        });

        assert_eq!(tray.title(), "Unavailable");
        assert_eq!(tray.statuses()[0].battery, "Battery: unavailable");
        assert_eq!(tray.statuses()[0].dpi, "DPI: unavailable");
    }

    #[test]
    fn nickname_replaces_the_reported_model_name() {
        let mut state = sample_state();
        state.device.nickname = Some("Desk mouse".to_owned());
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);

        assert_eq!(tray.statuses()[0].name, "Desk mouse");
        assert!(tray.tooltip().contains("Desk mouse"));
    }

    #[test]
    fn blank_nickname_falls_back_to_the_reported_name() {
        let mut state = sample_state();
        state.device.nickname = Some("   ".to_owned());
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);

        assert_eq!(tray.statuses()[0].name, "PRO X Superlight 2");
    }

    #[test]
    fn nickname_is_shown_while_a_mouse_is_unavailable() {
        let mut state = sample_state();
        state.device.nickname = Some("Desk mouse".to_owned());
        let id = state.device.id.clone();
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);
        tray.apply(AgentEvent::DeviceUnavailable {
            device_id: id,
            reason_code: Some(gflick_protocol::DeviceUnavailableReason::NotResponding),
            reason: "wireless link disconnected".to_owned(),
        });

        assert_eq!(tray.statuses()[0].name, "Desk mouse");
    }

    #[test]
    fn replace_preserves_snapshot_order() {
        let mut first = sample_state();
        first.device.id = "mouse-z".to_owned();
        first.device.nickname = Some("First".to_owned());
        let mut second = sample_state();
        second.device.id = "mouse-a".to_owned();
        second.device.nickname = Some("Second".to_owned());

        let mut tray = TrayState::default();
        tray.replace(vec![
            (first.device.clone(), Some(first)),
            (second.device.clone(), Some(second)),
        ]);

        assert_eq!(status_names(&tray), ["First", "Second"]);
    }

    #[test]
    fn ready_event_replaces_matching_device_in_place() {
        let mut first = sample_state();
        first.device.id = "mouse-z".to_owned();
        first.device.nickname = Some("First".to_owned());
        let mut second = sample_state();
        second.device.id = "mouse-a".to_owned();
        second.device.nickname = Some("Second".to_owned());
        let mut replacement = first.clone();
        replacement.device.nickname = Some("Updated first".to_owned());

        let mut tray = TrayState::default();
        tray.replace(vec![
            (first.device.clone(), Some(first)),
            (second.device.clone(), Some(second)),
        ]);
        tray.apply(AgentEvent::DeviceReady {
            device: Box::new(replacement),
        });

        assert_eq!(status_names(&tray), ["Updated first", "Second"]);
    }

    #[test]
    fn disconnected_event_preserves_survivor_order() {
        let mut first = sample_state();
        first.device.id = "mouse-z".to_owned();
        first.device.nickname = Some("First".to_owned());
        let mut second = sample_state();
        second.device.id = "mouse-b".to_owned();
        second.device.nickname = Some("Second".to_owned());
        let mut third = sample_state();
        third.device.id = "mouse-a".to_owned();
        third.device.nickname = Some("Third".to_owned());

        let mut tray = TrayState::default();
        tray.replace(vec![
            (first.device.clone(), Some(first)),
            (second.device.clone(), Some(second)),
            (third.device.clone(), Some(third)),
        ]);
        tray.apply(AgentEvent::DeviceDisconnected {
            device_id: "mouse-b".to_owned(),
        });

        assert_eq!(status_names(&tray), ["First", "Third"]);
    }

    #[test]
    fn connected_event_appends_new_device() {
        let mut first = sample_state();
        first.device.id = "mouse-z".to_owned();
        first.device.nickname = Some("First".to_owned());
        let mut second = sample_state();
        second.device.id = "mouse-a".to_owned();
        second.device.nickname = Some("Second".to_owned());
        let mut connected = sample_state().device;
        connected.id = "mouse-b".to_owned();
        connected.nickname = Some("Third".to_owned());

        let mut tray = TrayState::default();
        tray.replace(vec![
            (first.device.clone(), Some(first)),
            (second.device.clone(), Some(second)),
        ]);
        tray.apply(AgentEvent::DeviceConnected { device: connected });

        assert_eq!(status_names(&tray), ["First", "Second", "Third"]);
    }

    #[test]
    fn primary_device_selection_uses_snapshot_order() {
        let mut first = sample_state();
        first.device.id = "mouse-z".to_owned();
        first.device.nickname = Some("First".to_owned());
        first
            .settings
            .battery
            .as_mut()
            .expect("sample battery")
            .percentage = 91;
        first.settings.dpi.as_mut().expect("sample DPI").current_x = 1_200;
        let mut second = sample_state();
        second.device.id = "mouse-a".to_owned();
        second.device.nickname = Some("Second".to_owned());
        second
            .settings
            .battery
            .as_mut()
            .expect("sample battery")
            .percentage = 42;
        second.settings.dpi.as_mut().expect("sample DPI").current_x = 400;

        let mut tray = TrayState::default();
        tray.replace(vec![
            (first.device.clone(), Some(first)),
            (second.device.clone(), Some(second)),
        ]);

        assert!(tray.tooltip().contains("First"));
        assert_eq!(tray.title(), "91% · 1200 DPI");
    }

    #[test]
    fn metadata_rename_preserves_live_battery_and_dpi() {
        let state = sample_state();
        let mut renamed = state.device.clone();
        renamed.nickname = Some("Desk mouse".to_owned());
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);

        tray.apply(AgentEvent::DeviceMetadataChanged {
            devices: vec![renamed],
        });

        assert_eq!(tray.statuses()[0].name, "Desk mouse");
        assert_eq!(tray.title(), "84% · 800 DPI");
        assert!(tray.tooltip().contains("Battery: 84%"));
    }

    #[test]
    fn metadata_reorder_changes_the_ready_device_priority() {
        let mut first = sample_state();
        first.device.id = "mouse-first".to_owned();
        first.device.nickname = Some("First".to_owned());
        first
            .settings
            .battery
            .as_mut()
            .expect("sample battery")
            .percentage = 91;
        first.settings.dpi.as_mut().expect("sample DPI").current_x = 1_200;
        let mut second = sample_state();
        second.device.id = "mouse-second".to_owned();
        second.device.nickname = Some("Second".to_owned());
        second
            .settings
            .battery
            .as_mut()
            .expect("sample battery")
            .percentage = 42;
        second.settings.dpi.as_mut().expect("sample DPI").current_x = 400;
        let mut tray = TrayState::default();
        tray.replace(vec![
            (first.device.clone(), Some(first.clone())),
            (second.device.clone(), Some(second.clone())),
        ]);

        tray.apply(AgentEvent::DeviceMetadataChanged {
            devices: vec![second.device, first.device],
        });

        assert_eq!(status_names(&tray), ["Second", "First"]);
        assert_eq!(tray.title(), "42% · 400 DPI");
        assert!(tray.tooltip().contains("Second"));
    }

    #[test]
    fn metadata_rename_updates_an_offline_mouse_without_marking_it_connecting() {
        let state = sample_state();
        let mut offline = state.device;
        offline.ready = false;
        offline.availability = Some(DeviceAvailability::Unavailable {
            reason: gflick_protocol::DeviceUnavailableReason::NotResponding,
            detail: "mouse is asleep".to_owned(),
        });
        let mut renamed = offline.clone();
        renamed.nickname = Some("Desk mouse".to_owned());
        let mut tray = TrayState::default();
        tray.replace(vec![(offline, None)]);

        tray.apply(AgentEvent::DeviceMetadataChanged {
            devices: vec![renamed],
        });

        assert_eq!(tray.statuses()[0].name, "Desk mouse");
        assert_eq!(
            tray.statuses()[0].status.as_deref(),
            Some("mouse is asleep")
        );
    }

    #[test]
    fn metadata_snapshot_removes_devices_that_are_no_longer_present() {
        let mut first = sample_state();
        first.device.id = "mouse-first".to_owned();
        first.device.nickname = Some("First".to_owned());
        let mut second = sample_state();
        second.device.id = "mouse-second".to_owned();
        second.device.nickname = Some("Second".to_owned());
        let mut tray = TrayState::default();
        tray.replace(vec![
            (first.device.clone(), Some(first)),
            (second.device.clone(), Some(second.clone())),
        ]);

        tray.apply(AgentEvent::DeviceMetadataChanged {
            devices: vec![second.device],
        });

        assert_eq!(status_names(&tray), ["Second"]);
    }

    #[test]
    fn queued_connected_event_preserves_restored_offline_metadata() {
        let mut state = sample_state();
        state.device.nickname = Some("Desk mouse".to_owned());
        state.device.availability = Some(DeviceAvailability::Unavailable {
            reason: gflick_protocol::DeviceUnavailableReason::NotResponding,
            detail: "mouse is asleep".to_owned(),
        });
        state.device.ready = false;
        let snapshot = state.device.clone();
        let mut connected = snapshot.clone();
        connected.availability = Some(DeviceAvailability::Initializing);

        let mut tray = TrayState::default();
        tray.replace(vec![(snapshot, None)]);
        tray.apply(AgentEvent::DeviceConnected { device: connected });
        tray.apply(AgentEvent::DeviceUnavailable {
            device_id: state.device.id.clone(),
            reason_code: Some(gflick_protocol::DeviceUnavailableReason::NotResponding),
            reason: "mouse is asleep".to_owned(),
        });

        assert_eq!(tray.statuses()[0].name, "Desk mouse");
        assert_eq!(
            tray.devices
                .iter()
                .find(|device| device.summary.id == state.device.id)
                .expect("device should remain in the tray")
                .summary
                .display_name
                .as_deref(),
            Some("PRO X Superlight 2")
        );
        assert_eq!(tray.statuses()[0].battery, "Battery: unavailable");
        assert_eq!(tray.statuses()[0].dpi, "DPI: unavailable");
    }

    #[test]
    fn disconnected_mouse_is_removed_immediately() {
        let state = sample_state();
        let id = state.device.id.clone();
        let mut tray = TrayState::default();
        tray.replace(vec![(state.device.clone(), Some(state))]);
        tray.apply(AgentEvent::DeviceDisconnected { device_id: id });

        assert_eq!(tray.title(), "No mouse");
        assert!(tray.statuses().is_empty());
    }

    fn sample_state() -> DeviceState {
        DeviceState {
            device: DeviceSummary {
                id: "mouse-1".to_owned(),
                hardware_id: Some("hardware-1".to_owned()),
                vendor_id: 0x046d,
                product_id: 0xc54d,
                product_name: Some("USB Receiver".to_owned()),
                display_name: Some("PRO X Superlight 2".to_owned()),
                serial_number: None,
                nickname: None,
                color: Default::default(),
                sort_order: None,
                connection: DeviceConnection::Receiver,
                device_index: 1,
                availability: Some(DeviceAvailability::Ready),
                ready: true,
            },
            capabilities: DeviceCapabilities {
                battery: true,
                supported_dpi: Some(vec![800]),
                polling_rates: PollingRateCapabilities::Unsupported,
                onboard_profiles: false,
                onboard_profile_description: None,
                lift_off_distance: false,
                surface_mode: false,
                operating_mode_switch: false,
                color_led_effects: false,
                bunny_hopping: false,
                mouse_button_filter: false,
            },
            settings: SettingsState {
                onboard_dpi_stage: None,
                mouse_button_mapping: None,
                battery: Some(BatteryState {
                    percentage: 84,
                    level_code: 8,
                    status_code: 0,
                    status: "discharging".to_owned(),
                }),
                dpi: Some(DpiState {
                    current_x: 800,
                    default_x: 800,
                    current_y: Some(800),
                    default_y: Some(800),
                    lift_off_distance: None,
                }),
                polling_rate: PollingRateState::Unsupported,
                configuration_source: None,
                operating_mode: None,
                surface_mode: None,
                bunny_hopping: None,
            },
        }
    }

    fn status_names(tray: &TrayState) -> Vec<String> {
        tray.statuses()
            .into_iter()
            .map(|status| status.name)
            .collect()
    }
}
