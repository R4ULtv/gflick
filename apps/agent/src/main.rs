mod ipc;

use std::{
    collections::BTreeMap,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use open_hub_core as core;
use open_hub_protocol as protocol;

#[derive(Debug, Parser)]
#[command(name = "open-hub-agent", about = "Low-overhead Open Hub mouse agent")]
struct Cli {
    /// Seconds between USB device discovery passes.
    #[arg(long, default_value_t = 5)]
    scan_interval_seconds: u64,

    /// Seconds between battery queries for each open mouse.
    #[arg(long, default_value_t = 300)]
    battery_interval_seconds: u64,

    /// Discover devices, print their initial state, and exit without starting IPC.
    #[arg(long, conflicts_with = "request")]
    once: bool,

    /// Send one raw JSON request to a running agent and exit.
    #[arg(long, conflicts_with = "once")]
    request: Option<String>,

    /// Read one raw JSON request from standard input, send it, and exit.
    #[arg(long, conflicts_with_all = ["once", "request"])]
    request_stdin: bool,

    /// Subscribe to a running agent and print newline-delimited JSON events.
    #[arg(long, conflicts_with_all = ["once", "request", "request_stdin"])]
    events: bool,

    /// Exit the event client after this many events; useful for diagnostics.
    #[arg(long, requires = "events")]
    event_count: Option<usize>,
}

struct ActiveMouse {
    mouse: core::MouseDevice,
    battery: Option<core::BatteryInfo>,
    battery_check_after: std::time::Instant,
    battery_failures: u8,
    lighting_controlled: bool,
}

struct Agent {
    manager: core::DeviceManager,
    active: BTreeMap<String, ActiveMouse>,
    battery_interval: Duration,
}

impl Agent {
    fn new(battery_interval: Duration) -> Result<Self> {
        Ok(Self {
            manager: core::DeviceManager::new()?,
            active: BTreeMap::new(),
            battery_interval,
        })
    }

    fn tick(&mut self) -> Result<Vec<protocol::AgentEvent>> {
        let changes = self.manager.refresh_with_changes()?;
        let mut events = Vec::new();

        for device in &changes.disconnected {
            self.active.remove(&device.id);
            println!("Disconnected: {}", device_label(device));
            events.push(protocol::AgentEvent::DeviceDisconnected {
                device_id: device.id.clone(),
            });
        }

        for device in &changes.connected {
            println!("Connected: {}", device_label(device));
            events.push(protocol::AgentEvent::DeviceConnected {
                device: device_summary(device, false),
            });
        }

        // A receiver may be present while its mouse is asleep. Retry unopened
        // entries on later scans, but only report the initial failure once.
        let candidates = self.manager.devices().to_vec();
        for device in candidates {
            if self.active.contains_key(&device.id) {
                continue;
            }

            match self.manager.open(&device.id) {
                Ok(mouse) => match mouse.settings() {
                    Ok(settings) => {
                        print_initial_state(&device, &settings);
                        let state = device_state(&device, &mouse, Some(settings.clone()))?;
                        events.push(protocol::AgentEvent::DeviceReady {
                            device: Box::new(state),
                        });
                        self.active.insert(
                            device.id.clone(),
                            ActiveMouse {
                                mouse,
                                battery: settings.battery,
                                battery_check_after: std::time::Instant::now()
                                    + self.battery_interval,
                                battery_failures: 0,
                                lighting_controlled: false,
                            },
                        );
                    }
                    Err(error) => {
                        if changes
                            .connected
                            .iter()
                            .any(|connected| connected.id == device.id)
                        {
                            eprintln!(
                                "Could not read initial state for {}: {error:#}",
                                device_label(&device)
                            );
                        }
                    }
                },
                Err(error) => {
                    if changes
                        .connected
                        .iter()
                        .any(|connected| connected.id == device.id)
                    {
                        eprintln!("Could not open {}: {error:#}", device_label(&device));
                    }
                }
            }
        }

        let now = std::time::Instant::now();
        let mut unavailable = Vec::new();
        for (id, active) in &mut self.active {
            if now < active.battery_check_after {
                continue;
            }

            match active.mouse.battery() {
                Ok(battery) => {
                    active.battery_failures = 0;
                    if battery != active.battery {
                        println!("Battery [{id}]: {}", battery_label(battery));
                        active.battery = battery;
                        events.push(protocol::AgentEvent::BatteryChanged {
                            device_id: id.clone(),
                            battery: battery.map(battery_state),
                        });
                    }
                }
                Err(error) => {
                    active.battery_failures = active.battery_failures.saturating_add(1);
                    eprintln!(
                        "Battery query failed [{id}] ({}/3): {error:#}",
                        active.battery_failures
                    );
                    if active.battery_failures >= 3 {
                        unavailable.push((id.clone(), format!("{error:#}")));
                    }
                }
            }
            active.battery_check_after = now + self.battery_interval;
        }

        for (id, reason) in unavailable {
            self.active.remove(&id);
            events.push(protocol::AgentEvent::DeviceUnavailable {
                device_id: id,
                reason,
            });
        }

        Ok(events)
    }

    fn handle_request(&mut self, request: protocol::ClientRequest) -> protocol::ServerMessage {
        let id = request.id;
        if request.protocol_version != protocol::PROTOCOL_VERSION {
            return protocol::ServerMessage::error(
                id,
                protocol::ErrorCode::VersionMismatch,
                format!(
                    "client protocol version {} is incompatible with agent version {}",
                    request.protocol_version,
                    protocol::PROTOCOL_VERSION
                ),
            );
        }

        if let Some(device_id) = command_device_id(&request.command) {
            if self.manager.device(device_id).is_none() {
                return protocol::ServerMessage::error(
                    id,
                    protocol::ErrorCode::DeviceNotFound,
                    format!("device `{device_id}` is not connected"),
                );
            }
            if !self.active.contains_key(device_id) {
                return protocol::ServerMessage::error(
                    id,
                    protocol::ErrorCode::DeviceUnavailable,
                    format!("device `{device_id}` is present but not ready"),
                );
            }
        }

        match self.execute_command(request.command) {
            Ok(data) => protocol::ServerMessage::success(id, data),
            Err(error) => protocol::ServerMessage::error(
                id,
                protocol::ErrorCode::OperationFailed,
                format!("{error:#}"),
            ),
        }
    }

    fn execute_command(
        &mut self,
        command: protocol::RequestCommand,
    ) -> Result<protocol::ResponseData> {
        use protocol::RequestCommand;

        match command {
            RequestCommand::Ping => Ok(protocol::ResponseData::Pong),
            RequestCommand::ListDevices => Ok(protocol::ResponseData::Devices {
                devices: self
                    .manager
                    .devices()
                    .iter()
                    .map(|device| device_summary(device, self.active.contains_key(&device.id)))
                    .collect(),
            }),
            RequestCommand::GetDevice { device_id } => self.device_response(&device_id),
            RequestCommand::Subscribe => Ok(protocol::ResponseData::Subscribed),
            RequestCommand::SetDpi { device_id, dpi } => {
                self.mouse(&device_id)?.set_dpi(dpi)?;
                self.device_response(&device_id)
            }
            RequestCommand::SetPollingRate {
                device_id,
                connection,
                hz,
                disable_onboard_profiles,
            } => {
                let mouse = self.mouse(&device_id)?;
                if matches!(
                    mouse.configuration_source()?,
                    Some(core::ConfigurationSource::Onboard { .. })
                ) {
                    if !disable_onboard_profiles {
                        bail!(
                            "an onboard profile controls polling; set disable_onboard_profiles=true to switch to host control"
                        );
                    }
                    mouse.set_host_control()?;
                }
                mouse.set_polling_rate(connection_to_core(connection), hz)?;
                self.device_response(&device_id)
            }
            RequestCommand::SetLiftOffDistance { device_id, lod } => {
                self.mouse(&device_id)?
                    .set_lift_off_distance(lod_to_core(lod))?;
                self.device_response(&device_id)
            }
            RequestCommand::SetSurfaceMode { device_id, mode } => {
                self.mouse(&device_id)?
                    .set_surface_mode(surface_to_core(mode))?;
                self.device_response(&device_id)
            }
            RequestCommand::SetOperatingMode { device_id, mode } => {
                self.mouse(&device_id)?
                    .set_operating_mode(operating_to_core(mode))?;
                self.device_response(&device_id)
            }
            RequestCommand::SetBunnyHopping {
                device_id,
                enabled,
                timeout_ms,
            } => {
                self.mouse(&device_id)?
                    .set_bunny_hopping(enabled, timeout_ms)?;
                self.device_response(&device_id)
            }
            RequestCommand::SetLighting {
                device_id,
                zone,
                effect,
            } => {
                let active = self
                    .active
                    .get_mut(&device_id)
                    .with_context(|| format!("device `{device_id}` is not ready"))?;
                active
                    .mouse
                    .set_color_led_effect(zone, lighting_to_core(effect))?;
                active.lighting_controlled = true;
                self.device_response(&device_id)
            }
            RequestCommand::UseFirmwareLighting { device_id } => {
                let active = self
                    .active
                    .get_mut(&device_id)
                    .with_context(|| format!("device `{device_id}` is not ready"))?;
                active.mouse.release_color_led_control()?;
                active.lighting_controlled = false;
                self.device_response(&device_id)
            }
            RequestCommand::UseHostSettings { device_id } => {
                self.mouse(&device_id)?.set_host_control()?;
                self.device_response(&device_id)
            }
            RequestCommand::UseOnboardProfile { device_id, profile } => {
                self.mouse(&device_id)?.activate_onboard_profile(profile)?;
                self.device_response(&device_id)
            }
        }
    }

    fn mouse(&self, id: &str) -> Result<&core::MouseDevice> {
        self.active
            .get(id)
            .map(|active| &active.mouse)
            .with_context(|| format!("device `{id}` is not ready"))
    }

    fn device_response(&self, id: &str) -> Result<protocol::ResponseData> {
        let managed = self
            .manager
            .device(id)
            .with_context(|| format!("device `{id}` is not connected"))?;
        let mouse = self.mouse(id)?;
        Ok(protocol::ResponseData::Device {
            device: Box::new(device_state(managed, mouse, None)?),
        })
    }

    fn shutdown(&mut self) {
        for (id, active) in &mut self.active {
            if !active.lighting_controlled {
                continue;
            }
            match active.mouse.release_color_led_control() {
                Ok(()) => active.lighting_controlled = false,
                Err(error) => {
                    eprintln!("Could not return lighting control during shutdown [{id}]: {error:#}")
                }
            }
        }
    }
}

fn command_device_id(command: &protocol::RequestCommand) -> Option<&str> {
    use protocol::RequestCommand;
    match command {
        RequestCommand::Ping | RequestCommand::ListDevices | RequestCommand::Subscribe => None,
        RequestCommand::GetDevice { device_id }
        | RequestCommand::SetDpi { device_id, .. }
        | RequestCommand::SetPollingRate { device_id, .. }
        | RequestCommand::SetLiftOffDistance { device_id, .. }
        | RequestCommand::SetSurfaceMode { device_id, .. }
        | RequestCommand::SetOperatingMode { device_id, .. }
        | RequestCommand::SetBunnyHopping { device_id, .. }
        | RequestCommand::SetLighting { device_id, .. }
        | RequestCommand::UseFirmwareLighting { device_id }
        | RequestCommand::UseHostSettings { device_id }
        | RequestCommand::UseOnboardProfile { device_id, .. } => Some(device_id),
    }
}

fn command_changes_settings(command: &protocol::RequestCommand) -> bool {
    !matches!(
        command,
        protocol::RequestCommand::Ping
            | protocol::RequestCommand::ListDevices
            | protocol::RequestCommand::GetDevice { .. }
            | protocol::RequestCommand::Subscribe
    )
}

fn settings_event_from_response(
    response: &protocol::ServerMessage,
) -> Option<protocol::AgentEvent> {
    let protocol::ServerMessage::Response {
        result:
            protocol::ResponseResult::Success {
                data: protocol::ResponseData::Device { device },
            },
        ..
    } = response
    else {
        return None;
    };
    Some(protocol::AgentEvent::SettingsChanged {
        device: device.clone(),
    })
}

fn device_state(
    device: &core::ManagedDevice,
    mouse: &core::MouseDevice,
    settings: Option<core::SettingsSnapshot>,
) -> Result<protocol::DeviceState> {
    let capabilities = mouse.capabilities()?;
    let settings = settings.map_or_else(|| mouse.settings(), Ok)?;
    Ok(protocol::DeviceState {
        device: device_summary(device, true),
        capabilities: capabilities_state(capabilities),
        settings: settings_state(settings),
    })
}

fn device_summary(device: &core::ManagedDevice, ready: bool) -> protocol::DeviceSummary {
    protocol::DeviceSummary {
        id: device.id.clone(),
        vendor_id: device.vendor_id,
        product_id: device.product_id,
        product_name: device.product_name.clone(),
        serial_number: device.serial_number.clone(),
        connection: match device.connection {
            core::DeviceConnection::DirectUsb => protocol::DeviceConnection::DirectUsb,
            core::DeviceConnection::Receiver => protocol::DeviceConnection::Receiver,
        },
        device_index: device.device_index,
        ready,
    }
}

fn capabilities_state(capabilities: core::DeviceCapabilities) -> protocol::DeviceCapabilities {
    protocol::DeviceCapabilities {
        battery: capabilities.battery,
        supported_dpi: capabilities.supported_dpi,
        polling_rates: match capabilities.polling_rates {
            core::PollingRateCapabilities::Unsupported => {
                protocol::PollingRateCapabilities::Unsupported
            }
            core::PollingRateCapabilities::Shared { supported_hz } => {
                protocol::PollingRateCapabilities::Shared { supported_hz }
            }
            core::PollingRateCapabilities::PerConnection {
                wired_hz,
                wireless_hz,
            } => protocol::PollingRateCapabilities::PerConnection {
                wired_hz,
                wireless_hz,
            },
        },
        onboard_profiles: capabilities.onboard_profiles,
        onboard_profile_description: capabilities.onboard_profile_description.map(|description| {
            protocol::OnboardProfileDescription {
                memory_model_id: description.memory_model_id,
                profile_format_id: description.profile_format_id,
                macro_format_id: description.macro_format_id,
                profile_count: description.profile_count,
                factory_profile_count: description.factory_profile_count,
                button_count: description.button_count,
                sector_count: description.sector_count,
                sector_size: description.sector_size,
            }
        }),
        lift_off_distance: capabilities.lift_off_distance,
        surface_mode: capabilities.surface_mode,
        operating_mode_switch: capabilities.operating_mode_switch,
        color_led_effects: capabilities.color_led_effects,
        bunny_hopping: capabilities.bunny_hopping,
        mouse_button_filter: capabilities.mouse_button_filter,
    }
}

fn settings_state(settings: core::SettingsSnapshot) -> protocol::SettingsState {
    let (operating_mode, surface_mode) = settings.mode_status.map_or((None, None), |mode| {
        (
            Some(match mode.operating_mode() {
                core::OperatingMode::Endurance => protocol::OperatingMode::Endurance,
                core::OperatingMode::Performance => protocol::OperatingMode::Performance,
            }),
            mode.surface_mode.map(surface_from_core),
        )
    });
    protocol::SettingsState {
        battery: settings.battery.map(battery_state),
        dpi: settings.dpi.map(|dpi| protocol::DpiState {
            current_x: dpi.current_x,
            default_x: dpi.default_x,
            current_y: dpi.current_y,
            default_y: dpi.default_y,
            lift_off_distance: dpi.lod.map(lod_from_core),
        }),
        polling_rate: match settings.polling_rate {
            core::PollingRateSettings::Unsupported => protocol::PollingRateState::Unsupported,
            core::PollingRateSettings::Shared { hz } => protocol::PollingRateState::Shared { hz },
            core::PollingRateSettings::PerConnection {
                wired_hz,
                wireless_hz,
            } => protocol::PollingRateState::PerConnection {
                wired_hz,
                wireless_hz,
            },
        },
        configuration_source: settings.configuration_source.map(|source| match source {
            core::ConfigurationSource::Host => protocol::ConfigurationSource::Host,
            core::ConfigurationSource::Onboard { active_profile } => {
                protocol::ConfigurationSource::Onboard { active_profile }
            }
            core::ConfigurationSource::Unknown { raw_mode } => {
                protocol::ConfigurationSource::Unknown { raw_mode }
            }
        }),
        operating_mode,
        surface_mode,
        bunny_hopping: settings
            .bunny_hopping
            .map(|bhop| protocol::BunnyHoppingState {
                enabled: bhop.enabled,
                timeout_ms: bhop.timeout_ms,
            }),
    }
}

fn battery_state(battery: core::BatteryInfo) -> protocol::BatteryState {
    protocol::BatteryState {
        percentage: battery.percentage,
        level_code: battery.level_code,
        status_code: battery.status_code,
        status: battery.status_name().to_owned(),
    }
}

fn connection_to_core(value: protocol::Connection) -> core::ConnectionType {
    match value {
        protocol::Connection::Wired => core::ConnectionType::Wired,
        protocol::Connection::Wireless => core::ConnectionType::GamingWireless,
    }
}

fn lod_to_core(value: protocol::LiftOffDistance) -> core::LiftOffDistance {
    match value {
        protocol::LiftOffDistance::Low => core::LiftOffDistance::Low,
        protocol::LiftOffDistance::Medium => core::LiftOffDistance::Medium,
        protocol::LiftOffDistance::High => core::LiftOffDistance::High,
    }
}

fn lod_from_core(value: core::LiftOffDistance) -> protocol::LiftOffDistance {
    match value {
        core::LiftOffDistance::Low => protocol::LiftOffDistance::Low,
        core::LiftOffDistance::Medium => protocol::LiftOffDistance::Medium,
        core::LiftOffDistance::High => protocol::LiftOffDistance::High,
    }
}

fn surface_to_core(value: protocol::SurfaceMode) -> core::SurfaceMode {
    match value {
        protocol::SurfaceMode::On => core::SurfaceMode::On,
        protocol::SurfaceMode::Automatic => core::SurfaceMode::Automatic,
        protocol::SurfaceMode::Off => core::SurfaceMode::Off,
    }
}

fn surface_from_core(value: core::SurfaceMode) -> protocol::SurfaceMode {
    match value {
        core::SurfaceMode::On => protocol::SurfaceMode::On,
        core::SurfaceMode::Automatic => protocol::SurfaceMode::Automatic,
        core::SurfaceMode::Off => protocol::SurfaceMode::Off,
    }
}

fn operating_to_core(value: protocol::OperatingMode) -> core::OperatingMode {
    match value {
        protocol::OperatingMode::Endurance => core::OperatingMode::Endurance,
        protocol::OperatingMode::Performance => core::OperatingMode::Performance,
    }
}

fn lighting_to_core(value: protocol::LightingEffect) -> core::ColorLedEffect {
    match value {
        protocol::LightingEffect::Disabled => core::ColorLedEffect::Disabled,
        protocol::LightingEffect::Fixed { color } => core::ColorLedEffect::Fixed {
            color: core::RgbColor {
                red: color.red,
                green: color.green,
                blue: color.blue,
            },
        },
        protocol::LightingEffect::Cycling {
            period_ms,
            brightness,
        } => core::ColorLedEffect::Cycling {
            period_ms,
            brightness,
        },
        protocol::LightingEffect::Breathing {
            color,
            period_ms,
            brightness,
        } => core::ColorLedEffect::Breathing {
            color: core::RgbColor {
                red: color.red,
                green: color.green,
                blue: color.blue,
            },
            period_ms,
            brightness,
        },
    }
}

fn device_label(device: &core::ManagedDevice) -> String {
    let name = device.product_name.as_deref().unwrap_or("Logitech mouse");
    let connection = match device.connection {
        core::DeviceConnection::DirectUsb => "direct USB",
        core::DeviceConnection::Receiver => "receiver",
    };
    format!(
        "{name} {:04x}:{:04x} ({connection}, {})",
        device.vendor_id, device.product_id, device.id
    )
}

fn battery_label(battery: Option<core::BatteryInfo>) -> String {
    battery.map_or_else(
        || "unsupported".to_owned(),
        |battery| format!("{}% ({})", battery.percentage, battery.status_name()),
    )
}

fn print_initial_state(device: &core::ManagedDevice, settings: &core::SettingsSnapshot) {
    println!("  Ready: {}", device.id);
    println!("  Battery: {}", battery_label(settings.battery));
    match settings.dpi {
        Some(dpi) => println!(
            "  DPI: X={}, Y={}, LOD={}",
            dpi.current_x,
            dpi.current_y
                .map_or_else(|| "n/a".to_owned(), |y| y.to_string()),
            dpi.lod
                .map_or_else(|| "n/a".to_owned(), |lod| lod.to_string())
        ),
        None => println!("  DPI: unsupported"),
    }
    match settings.polling_rate {
        core::PollingRateSettings::Unsupported => println!("  Polling rate: unsupported"),
        core::PollingRateSettings::Shared { hz } => {
            println!("  Polling rate (shared): {hz} Hz")
        }
        core::PollingRateSettings::PerConnection {
            wired_hz,
            wireless_hz,
        } => println!("  Polling rate: wired {wired_hz} Hz, wireless {wireless_hz} Hz"),
    }
    match settings.configuration_source {
        Some(core::ConfigurationSource::Host) => {
            println!("  Configuration: host/local settings")
        }
        Some(core::ConfigurationSource::Onboard { active_profile }) => match active_profile {
            Some(profile) => println!("  Configuration: onboard profile 0x{profile:04x}"),
            None => println!("  Configuration: onboard profile"),
        },
        Some(core::ConfigurationSource::Unknown { raw_mode }) => {
            println!("  Configuration: unknown mode 0x{raw_mode:02x}")
        }
        None => println!("  Configuration: unavailable"),
    }
    if let Some(mode) = settings.mode_status {
        if let Some(surface_mode) = mode.surface_mode {
            println!("  Surface mode: {surface_mode}");
        }
    }
    if let Some(bhop) = settings.bunny_hopping {
        println!(
            "  BHOP: {} ({})",
            if bhop.enabled { "on" } else { "off" },
            if bhop.timeout_ms == 0 {
                "timeout not configured".to_owned()
            } else {
                format!("timeout {} ms", bhop.timeout_ms)
            }
        );
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(request) = cli.request {
        println!("{}", ipc::send_request(&request)?);
        return Ok(());
    }
    if cli.request_stdin {
        let mut request = String::new();
        std::io::stdin().read_to_string(&mut request)?;
        println!("{}", ipc::send_request(request.trim_end())?);
        return Ok(());
    }
    if cli.events {
        if cli.event_count == Some(0) {
            bail!("--event-count must be greater than zero");
        }
        return ipc::print_event_stream(cli.event_count);
    }
    if cli.scan_interval_seconds == 0 {
        bail!("--scan-interval-seconds must be greater than zero");
    }
    if cli.battery_interval_seconds == 0 {
        bail!("--battery-interval-seconds must be greater than zero");
    }

    let scan_interval = Duration::from_secs(cli.scan_interval_seconds);
    let mut agent = Agent::new(Duration::from_secs(cli.battery_interval_seconds))?;

    if cli.once {
        println!("Open Hub agent one-shot discovery; no settings will be changed.");
        agent.tick()?;
        return Ok(());
    }

    let ipc = ipc::start()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_signal = Arc::clone(&shutdown);
    ctrlc::set_handler(move || shutdown_signal.store(true, Ordering::Release))
        .context("failed to install the graceful-shutdown handler")?;
    println!(
        "Open Hub agent started; IPC protocol v{} is ready.",
        protocol::PROTOCOL_VERSION
    );
    let mut next_scan = std::time::Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        let now = std::time::Instant::now();
        if now >= next_scan {
            match agent.tick() {
                Ok(events) => {
                    for event in events {
                        ipc.publish(event);
                    }
                }
                Err(error) => eprintln!("Discovery pass failed: {error:#}"),
            }
            next_scan = std::time::Instant::now() + scan_interval;
        }

        while let Some(pending) = ipc.try_recv()? {
            let publish_change = command_changes_settings(&pending.request.command);
            let response = agent.handle_request(pending.request);
            if publish_change {
                if let Some(event) = settings_event_from_response(&response) {
                    ipc.publish(event);
                }
            }
            let _ = pending.reply.send(response);
        }

        let wait = next_scan
            .saturating_duration_since(std::time::Instant::now())
            .min(Duration::from_secs(1));
        if let Some(pending) = ipc.recv_timeout(wait)? {
            let publish_change = command_changes_settings(&pending.request.command);
            let response = agent.handle_request(pending.request);
            if publish_change {
                if let Some(event) = settings_event_from_response(&response) {
                    ipc.publish(event);
                }
            }
            let _ = pending.reply.send(response);
        }
    }
    agent.shutdown();
    println!("Open Hub agent stopped cleanly.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_device_ids_only_from_device_commands() {
        assert_eq!(command_device_id(&protocol::RequestCommand::Ping), None);
        assert_eq!(
            command_device_id(&protocol::RequestCommand::SetDpi {
                device_id: "mouse-1".to_owned(),
                dpi: 800,
            }),
            Some("mouse-1")
        );
    }

    #[test]
    fn only_mutations_emit_settings_events() {
        assert!(!command_changes_settings(
            &protocol::RequestCommand::ListDevices
        ));
        assert!(command_changes_settings(
            &protocol::RequestCommand::SetDpi {
                device_id: "mouse-1".to_owned(),
                dpi: 800,
            }
        ));
    }
}
