use std::collections::BTreeMap;

use open_hub_protocol::{
    AgentEvent, BatteryState, DeviceState, DeviceSummary, DpiState, SettingsState,
};

#[derive(Debug, Default)]
pub struct TrayState {
    pub agent_connected: bool,
    devices: BTreeMap<String, DeviceView>,
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
                let id = summary.id.clone();
                let view = state.map_or_else(
                    || DeviceView {
                        unavailable_reason: availability_reason(&summary),
                        summary,
                        settings: None,
                    },
                    DeviceView::from_state,
                );
                (id, view)
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
                self.devices.insert(
                    device.id.clone(),
                    DeviceView {
                        unavailable_reason: availability_reason(&device),
                        summary: device,
                        settings: None,
                    },
                );
            }
            AgentEvent::DeviceReady { device } | AgentEvent::SettingsChanged { device } => {
                self.devices
                    .insert(device.device.id.clone(), DeviceView::from_state(*device));
            }
            AgentEvent::DeviceUnavailable {
                device_id, reason, ..
            } => {
                if let Some(device) = self.devices.get_mut(&device_id) {
                    device.settings = None;
                    device.unavailable_reason = Some(reason);
                }
            }
            AgentEvent::DeviceDisconnected { device_id } => {
                self.devices.remove(&device_id);
            }
            AgentEvent::BatteryChanged { device_id, battery } => {
                if let Some(settings) = self
                    .devices
                    .get_mut(&device_id)
                    .and_then(|device| device.settings.as_mut())
                {
                    settings.battery = battery;
                }
            }
        }
    }

    pub fn statuses(&self) -> Vec<DeviceStatus> {
        self.devices.values().map(DeviceView::status).collect()
    }

    pub fn tooltip(&self) -> String {
        if !self.agent_connected {
            return "Open Hub — agent offline".to_owned();
        }
        let Some(device) = self
            .devices
            .values()
            .find(|device| device.settings.is_some())
        else {
            return if self.devices.is_empty() {
                "Open Hub — no mouse connected".to_owned()
            } else {
                "Open Hub — mouse unavailable".to_owned()
            };
        };
        let status = device.status();
        truncate(
            &format!(
                "Open Hub — {}\n{} · {}",
                status.name, status.battery, status.dpi
            ),
            120,
        )
    }

    pub fn title(&self) -> String {
        if !self.agent_connected {
            return "Offline".to_owned();
        }
        let Some(device) = self
            .devices
            .values()
            .find(|device| device.settings.is_some())
        else {
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
        let name = self
            .summary
            .display_name
            .clone()
            .or_else(|| self.summary.product_name.clone())
            .unwrap_or_else(|| {
                format!(
                    "Mouse {:04x}:{:04x}",
                    self.summary.vendor_id, self.summary.product_id
                )
            });
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
        Some(open_hub_protocol::DeviceAvailability::Unavailable { detail, .. }) => {
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
    use open_hub_protocol::{
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
            reason_code: Some(open_hub_protocol::DeviceUnavailableReason::NotResponding),
            reason: "wireless link disconnected".to_owned(),
        });

        assert_eq!(tray.title(), "Unavailable");
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
}
