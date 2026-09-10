use std::io::Write;

use anyhow::Result;
use gflick_protocol::{
    AgentEvent, ConfigurationSource, DeviceState, DeviceSummary, OperatingMode, PROTOCOL_VERSION,
    PollingRateState, SurfaceMode,
};

use crate::{
    args::OutputFormat,
    selector::{device_name, device_status, stable_selector},
};

pub fn write_status(mut writer: impl Write, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Human => writeln!(writer, "Agent: running (protocol {PROTOCOL_VERSION})")?,
        OutputFormat::Json => write_json_line(
            writer,
            &serde_json::json!({
                "agent": "running",
                "protocol_version": PROTOCOL_VERSION,
                "version": env!("CARGO_PKG_VERSION"),
            }),
        )?,
    }
    Ok(())
}

pub fn write_devices(
    mut writer: impl Write,
    format: OutputFormat,
    devices: &[DeviceSummary],
) -> Result<()> {
    match format {
        OutputFormat::Human => {
            if devices.is_empty() {
                writeln!(writer, "No devices found.")?;
            }
            for device in devices {
                writeln!(
                    writer,
                    "{} ({}) — {}",
                    device_name(device),
                    stable_selector(device),
                    device_status(device)
                )?;
            }
        }
        OutputFormat::Json => write_json_line(writer, &serde_json::json!({ "devices": devices }))?,
    }
    Ok(())
}

pub fn write_device(
    mut writer: impl Write,
    format: OutputFormat,
    device: &DeviceState,
) -> Result<()> {
    match format {
        OutputFormat::Human => write_human_device(&mut writer, device)?,
        OutputFormat::Json => write_json_line(writer, &serde_json::json!({ "device": device }))?,
    }
    Ok(())
}

fn write_human_device(writer: &mut impl Write, device: &DeviceState) -> Result<()> {
    writeln!(writer, "Device: {}", device_name(&device.device))?;
    writeln!(writer, "Selector: {}", stable_selector(&device.device))?;
    writeln!(writer, "Status: {}", device_status(&device.device))?;
    if !device.device.ready {
        return Ok(());
    }

    if let Some(battery) = &device.settings.battery {
        writeln!(
            writer,
            "Battery: {}% ({})",
            battery.percentage, battery.status
        )?;
    }
    if let Some(dpi) = device.settings.dpi {
        match dpi.current_y {
            Some(y) => writeln!(writer, "DPI: {} x {y}", dpi.current_x)?,
            None => writeln!(writer, "DPI: {}", dpi.current_x)?,
        }
    }
    match device.settings.polling_rate {
        PollingRateState::Unsupported => {}
        PollingRateState::Shared { hz } => writeln!(writer, "Polling rate: {hz} Hz")?,
        PollingRateState::PerConnection {
            wired_hz,
            wireless_hz,
        } => writeln!(
            writer,
            "Polling rate: wired {wired_hz} Hz; wireless {wireless_hz} Hz"
        )?,
    }
    if let Some(source) = device.settings.configuration_source {
        writeln!(
            writer,
            "Configuration source: {}",
            configuration_source(source)
        )?;
    }
    if let Some(mode) = device.settings.operating_mode {
        writeln!(writer, "Operating mode: {}", operating_mode(mode))?;
    }
    if let Some(mode) = device.settings.surface_mode {
        writeln!(writer, "Surface mode: {}", surface_mode(mode))?;
    }
    if let Some(bhop) = device.settings.bunny_hopping {
        let state = if bhop.enabled { "on" } else { "off" };
        writeln!(writer, "BHOP: {state} ({} ms)", bhop.timeout_ms)?;
    }
    Ok(())
}

fn configuration_source(source: ConfigurationSource) -> String {
    match source {
        ConfigurationSource::Host => "host".to_owned(),
        ConfigurationSource::Onboard { active_profile } => active_profile
            .map(|profile| format!("onboard profile {profile}"))
            .unwrap_or_else(|| "onboard".to_owned()),
        ConfigurationSource::Unknown { raw_mode } => format!("unknown ({raw_mode})"),
    }
}

const fn operating_mode(mode: OperatingMode) -> &'static str {
    match mode {
        OperatingMode::Performance => "performance",
        OperatingMode::Endurance => "endurance",
    }
}

const fn surface_mode(mode: SurfaceMode) -> &'static str {
    match mode {
        SurfaceMode::On => "on",
        SurfaceMode::Automatic => "automatic",
        SurfaceMode::Off => "off",
    }
}

pub fn write_event(mut writer: impl Write, format: OutputFormat, event: &AgentEvent) -> Result<()> {
    match format {
        OutputFormat::Human => writeln!(writer, "{}", event_human(event))?,
        OutputFormat::Json => write_json_line(&mut writer, event)?,
    }
    writer.flush()?;
    Ok(())
}

fn write_json_line(mut writer: impl Write, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(&mut writer, value)?;
    writeln!(writer)?;
    Ok(())
}

fn event_human(event: &AgentEvent) -> String {
    match event {
        AgentEvent::ApplicationShuttingDown => "Agent is shutting down".to_owned(),
        AgentEvent::DeviceConnected { device } => {
            format!("Device connected: {}", device_name(device))
        }
        AgentEvent::DeviceReady { device } => {
            format!("Device ready: {}", device_name(&device.device))
        }
        AgentEvent::DeviceUnavailable {
            device_id, reason, ..
        } => format!("Device unavailable: {device_id} ({reason})"),
        AgentEvent::SettingsChanged { device } => {
            format!("Settings changed: {}", device_name(&device.device))
        }
        AgentEvent::DeviceMetadataChanged { devices } => {
            format!("Device metadata changed: {} device(s)", devices.len())
        }
        AgentEvent::DeviceDisconnected { device_id } => format!("Device disconnected: {device_id}"),
        AgentEvent::BatteryChanged { device_id, battery } => match battery {
            Some(battery) => format!("Battery changed: {device_id} ({}%)", battery.percentage),
            None => format!("Battery unavailable: {device_id}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use gflick_protocol::{
        BatteryState, BunnyHoppingState, DeviceAvailability, DeviceConnection, DeviceSummary,
        DpiState,
    };

    use super::*;

    fn summary() -> DeviceSummary {
        DeviceSummary {
            id: "mouse-1".to_owned(),
            hardware_id: None,
            vendor_id: 1,
            product_id: 2,
            product_name: None,
            display_name: None,
            serial_number: None,
            nickname: None,
            color: Default::default(),
            sort_order: None,
            connection: DeviceConnection::DirectUsb,
            device_index: 0,
            availability: None,
            ready: true,
        }
    }

    #[test]
    fn devices_json_is_valid_without_prose() {
        let mut output = Vec::new();
        write_devices(&mut output, OutputFormat::Json, &[summary()]).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert!(value["devices"].is_array());
    }

    #[test]
    fn status_and_device_json_use_the_documented_wrappers() {
        let mut status = Vec::new();
        write_status(&mut status, OutputFormat::Json).unwrap();
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        assert_eq!(status["agent"], "running");
        assert!(status["protocol_version"].is_number());
        assert!(status["version"].is_string());

        let mut device = Vec::new();
        write_device(&mut device, OutputFormat::Json, &state()).unwrap();
        let device: serde_json::Value = serde_json::from_slice(&device).unwrap();
        assert!(device["device"].is_object());
    }

    #[test]
    fn human_list_uses_vid_pid_fallback_and_status() {
        let mut output = Vec::new();
        write_devices(&mut output, OutputFormat::Human, &[summary()]).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "0001:0002 (mouse-1) — ready\n"
        );
    }

    #[test]
    fn human_list_renders_unavailable_status_and_stable_selector() {
        let mut unavailable = summary();
        unavailable.hardware_id = Some("hardware-id".to_owned());
        unavailable.availability = Some(DeviceAvailability::Unavailable {
            reason: gflick_protocol::DeviceUnavailableReason::NotResponding,
            detail: "sleeping".to_owned(),
        });
        unavailable.ready = false;
        let mut output = Vec::new();
        write_devices(&mut output, OutputFormat::Human, &[unavailable]).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "0001:0002 (hardware-id) — unavailable: sleeping\n"
        );
    }

    #[test]
    fn human_device_snapshot_contains_available_settings() {
        let mut output = Vec::new();
        write_device(&mut output, OutputFormat::Human, &populated_state()).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Device: 0001:0002\n"));
        assert!(output.contains("Selector: mouse-1\nStatus: ready\n"));
        assert!(output.contains("Battery: 89% (discharging)\n"));
        assert!(output.contains("DPI: 1600 x 800\n"));
        assert!(output.contains("Polling rate: wired 1000 Hz; wireless 500 Hz\n"));
        assert!(output.contains("Configuration source: onboard profile 2\n"));
        assert!(output.contains("Operating mode: performance\nSurface mode: automatic\n"));
        assert!(output.contains("BHOP: on (100 ms)\n"));
    }

    #[test]
    fn unavailable_device_omits_stale_settings() {
        let mut unavailable = state();
        unavailable.device.ready = false;
        unavailable.device.availability = Some(DeviceAvailability::Unavailable {
            reason: gflick_protocol::DeviceUnavailableReason::NotResponding,
            detail: "sleeping".to_owned(),
        });
        unavailable.settings.battery = Some(BatteryState {
            percentage: 89,
            level_code: 8,
            status_code: 0,
            status: "discharging".to_owned(),
        });
        let mut output = Vec::new();
        write_device(&mut output, OutputFormat::Human, &unavailable).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Device: 0001:0002\nSelector: mouse-1\nStatus: unavailable: sleeping\n"
        );
    }

    #[test]
    fn all_events_format_in_human_and_json() {
        let events = [
            AgentEvent::ApplicationShuttingDown,
            AgentEvent::DeviceConnected { device: summary() },
            AgentEvent::DeviceReady {
                device: Box::new(state()),
            },
            AgentEvent::DeviceUnavailable {
                device_id: "mouse-1".to_owned(),
                reason_code: None,
                reason: "sleeping".to_owned(),
            },
            AgentEvent::SettingsChanged {
                device: Box::new(state()),
            },
            AgentEvent::DeviceMetadataChanged {
                devices: vec![summary()],
            },
            AgentEvent::DeviceDisconnected {
                device_id: "mouse-1".to_owned(),
            },
            AgentEvent::BatteryChanged {
                device_id: "mouse-1".to_owned(),
                battery: None,
            },
            AgentEvent::BatteryChanged {
                device_id: "mouse-1".to_owned(),
                battery: Some(BatteryState {
                    percentage: 89,
                    level_code: 8,
                    status_code: 0,
                    status: "discharging".to_owned(),
                }),
            },
        ];
        for event in events {
            let mut json = Vec::new();
            write_event(&mut json, OutputFormat::Json, &event).unwrap();
            assert!(serde_json::from_slice::<serde_json::Value>(&json).is_ok());
            let mut human = Vec::new();
            write_event(&mut human, OutputFormat::Human, &event).unwrap();
            assert!(!human.is_empty());
        }
    }

    #[test]
    fn metadata_event_has_compact_human_and_canonical_json_output() {
        let event = AgentEvent::DeviceMetadataChanged {
            devices: vec![summary()],
        };
        let mut human = Vec::new();
        write_event(&mut human, OutputFormat::Human, &event).unwrap();
        assert_eq!(
            String::from_utf8(human).unwrap(),
            "Device metadata changed: 1 device(s)\n"
        );

        let mut json = Vec::new();
        write_event(&mut json, OutputFormat::Json, &event).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(json["event"], "device_metadata_changed");
        assert_eq!(json["devices"][0]["id"], "mouse-1");
    }

    fn state() -> DeviceState {
        use gflick_protocol::{
            DeviceCapabilities, PollingRateCapabilities, PollingRateState, SettingsState,
        };
        DeviceState {
            device: summary(),
            capabilities: DeviceCapabilities {
                battery: false,
                supported_dpi: None,
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
                battery: None,
                dpi: None,
                polling_rate: PollingRateState::Unsupported,
                configuration_source: None,
                operating_mode: None,
                surface_mode: None,
                bunny_hopping: None,
            },
        }
    }

    fn populated_state() -> DeviceState {
        use gflick_protocol::{ConfigurationSource, OperatingMode, PollingRateState, SurfaceMode};
        let mut device = state();
        device.settings.battery = Some(BatteryState {
            percentage: 89,
            level_code: 8,
            status_code: 0,
            status: "discharging".to_owned(),
        });
        device.settings.dpi = Some(DpiState {
            current_x: 1600,
            default_x: 800,
            current_y: Some(800),
            default_y: None,
            lift_off_distance: None,
        });
        device.settings.polling_rate = PollingRateState::PerConnection {
            wired_hz: 1000,
            wireless_hz: 500,
        };
        device.settings.configuration_source = Some(ConfigurationSource::Onboard {
            active_profile: Some(2),
        });
        device.settings.operating_mode = Some(OperatingMode::Performance);
        device.settings.surface_mode = Some(SurfaceMode::Automatic);
        device.settings.bunny_hopping = Some(BunnyHoppingState {
            enabled: true,
            timeout_ms: 100,
        });
        device
    }
}
