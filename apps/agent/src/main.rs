mod ipc;
mod startup;
mod store;

use std::{
    collections::BTreeMap,
    io::Read,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use open_hub_core as core;
use open_hub_protocol as protocol;

#[derive(Debug, Parser)]
#[command(name = "open-hub-agent", about = "Low-overhead Open Hub mouse agent")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,

    /// Seconds between USB device discovery passes.
    #[arg(long, default_value_t = 5)]
    scan_interval_seconds: u64,

    /// Seconds between battery queries for each open mouse.
    #[arg(long, default_value_t = 300)]
    battery_interval_seconds: u64,

    /// Override the per-user JSON settings file path.
    #[arg(long, value_name = "PATH")]
    settings_file: Option<PathBuf>,

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

    /// Internal Windows login launcher that starts the agent and tray together.
    #[cfg(windows)]
    #[arg(long, hide = true)]
    launch_background: bool,

    /// Internal detached Windows agent process.
    #[cfg(windows)]
    #[arg(long, hide = true)]
    background_worker: bool,

    /// Internal macOS login mode that starts the tray before running the agent.
    #[cfg(target_os = "macos")]
    #[arg(long, hide = true)]
    launch_tray: bool,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Manage automatic startup for the current user.
    Startup {
        #[command(subcommand)]
        action: StartupAction,
    },
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum StartupAction {
    /// Install the agent and sibling tray, then start both automatically at login.
    Install,
    /// Remove automatic startup and both installed binaries.
    Uninstall,
    /// Show the current per-user startup registration.
    Status,
}

struct ActiveMouse {
    mouse: core::MouseDevice,
    hardware_id: Option<String>,
    display_name: Option<String>,
    capabilities: core::DeviceCapabilities,
    battery: Option<core::BatteryInfo>,
    battery_check_after: std::time::Instant,
    battery_failures: u8,
    lighting_controlled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnavailableDevice {
    reason: protocol::DeviceUnavailableReason,
    detail: String,
    hardware_id: Option<String>,
}

impl UnavailableDevice {
    fn availability(&self) -> protocol::DeviceAvailability {
        protocol::DeviceAvailability::Unavailable {
            reason: self.reason,
            detail: self.detail.clone(),
        }
    }
}

struct Agent {
    manager: core::DeviceManager,
    active: BTreeMap<String, ActiveMouse>,
    unavailable: BTreeMap<String, UnavailableDevice>,
    battery_interval: Duration,
    settings: store::SettingsStore,
    restore_preferences: bool,
}

impl Agent {
    fn new(
        battery_interval: Duration,
        settings_path: PathBuf,
        restore_preferences: bool,
    ) -> Result<Self> {
        Ok(Self {
            manager: core::DeviceManager::new()?,
            active: BTreeMap::new(),
            unavailable: BTreeMap::new(),
            battery_interval,
            settings: store::SettingsStore::load(settings_path)?,
            restore_preferences,
        })
    }

    fn prepare_mouse(
        &mut self,
        device: &core::ManagedDevice,
        mouse: core::MouseDevice,
    ) -> Result<(ActiveMouse, protocol::DeviceState)> {
        let mut settings = mouse.settings()?;
        let capabilities = mouse.capabilities()?;
        let hardware_id = match mouse.hardware_id() {
            Ok(hardware_id) => hardware_id,
            Err(error) => {
                eprintln!(
                    "Could not read persistent hardware identity for {}: {error:#}",
                    device_label(device)
                );
                None
            }
        };
        let display_name = match mouse.name() {
            Ok(name) => name,
            Err(error) => {
                eprintln!(
                    "Could not read device name for {}: {error:#}",
                    device_label(device)
                );
                None
            }
        };
        let mut lighting_controlled = false;

        if self.restore_preferences {
            if let Some(hardware_id) = hardware_id.as_deref() {
                if let Some(preferences) = self.settings.device(hardware_id).cloned() {
                    match restore_device_preferences(
                        &mouse,
                        device.connection_type(),
                        &capabilities,
                        &settings,
                        &preferences,
                    ) {
                        Ok(controlled) => {
                            lighting_controlled = controlled;
                            println!("  Restored saved preferences: {hardware_id}");
                        }
                        Err(error) => {
                            eprintln!(
                                "Could not fully restore settings for {hardware_id}: {error:#}"
                            );
                            if matches!(
                                preferences.lighting,
                                Some(store::LightingPreference::Software { .. })
                            ) {
                                let _ = mouse.release_color_led_control();
                            }
                        }
                    }
                    settings = mouse.settings()?;
                } else {
                    let preferences = capture_device_preferences(&capabilities, &settings);
                    if self.settings.insert_if_missing(hardware_id, preferences) {
                        if let Err(error) = self.settings.save() {
                            eprintln!(
                                "Could not save initial settings for {hardware_id} to `{}`: {error:#}",
                                self.settings.path().display()
                            );
                        } else {
                            println!(
                                "  Saved initial preferences: {}",
                                self.settings.path().display()
                            );
                        }
                    }
                }
            }
        }

        print_initial_state(device, hardware_id.as_deref(), &settings);
        let state = device_state(
            device,
            &mouse,
            hardware_id.as_deref(),
            display_name.as_deref(),
            &capabilities,
            Some(settings.clone()),
        )?;
        let active = ActiveMouse {
            mouse,
            hardware_id,
            display_name,
            capabilities,
            battery: settings.battery,
            battery_check_after: std::time::Instant::now() + self.battery_interval,
            battery_failures: 0,
            lighting_controlled,
        };
        Ok((active, state))
    }

    fn tick(&mut self) -> Result<Vec<protocol::AgentEvent>> {
        let changes = self.manager.refresh_with_changes()?;
        let mut events = Vec::new();

        for device in &changes.disconnected {
            self.active.remove(&device.id);
            self.unavailable.remove(&device.id);
            println!("Disconnected: {}", device_label(device));
            events.push(protocol::AgentEvent::DeviceDisconnected {
                device_id: device.id.clone(),
            });
        }

        for device in &changes.connected {
            println!("Connected: {}", device_label(device));
            events.push(protocol::AgentEvent::DeviceConnected {
                device: device_summary(
                    device,
                    protocol::DeviceAvailability::Initializing,
                    None,
                    None,
                ),
            });
        }

        // A USB interface may remain present while a mouse is asleep or switched
        // off. Keep retrying, but emit unavailable only when its reason changes.
        let candidates = self.manager.devices().to_vec();
        for device in candidates {
            if self.active.contains_key(&device.id) {
                continue;
            }

            match self.manager.open(&device.id) {
                Ok(mouse) => match self.prepare_mouse(&device, mouse) {
                    Ok((active, state)) => {
                        self.unavailable.remove(&device.id);
                        events.push(protocol::AgentEvent::DeviceReady {
                            device: Box::new(state),
                        });
                        self.active.insert(device.id.clone(), active);
                    }
                    Err(error) => {
                        self.mark_unavailable(
                            &device.id,
                            protocol::DeviceUnavailableReason::CommunicationError,
                            format!("could not read mouse state: {error:#}"),
                            None,
                            &mut events,
                        );
                    }
                },
                Err(error) => {
                    self.mark_unavailable(
                        &device.id,
                        protocol::DeviceUnavailableReason::NotResponding,
                        format!(
                            "mouse is present but not responding; it may be asleep or switched off: {error:#}"
                        ),
                        None,
                        &mut events,
                    );
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
                        unavailable.push((
                            id.clone(),
                            format!("mouse stopped responding during a battery query: {error:#}"),
                            active.hardware_id.clone(),
                        ));
                    }
                }
            }
            active.battery_check_after = now + self.battery_interval;
        }

        for (id, detail, hardware_id) in unavailable {
            self.active.remove(&id);
            self.mark_unavailable(
                &id,
                protocol::DeviceUnavailableReason::CommunicationError,
                detail,
                hardware_id,
                &mut events,
            );
        }

        Ok(events)
    }

    fn mark_unavailable(
        &mut self,
        device_id: &str,
        reason: protocol::DeviceUnavailableReason,
        detail: String,
        hardware_id: Option<String>,
        events: &mut Vec<protocol::AgentEvent>,
    ) {
        if let Some(event) = record_unavailable(
            &mut self.unavailable,
            device_id,
            reason,
            detail.clone(),
            hardware_id,
        ) {
            eprintln!("Unavailable [{device_id}]: {detail}");
            events.push(event);
        }
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
                let detail = self
                    .unavailable
                    .get(device_id)
                    .map(|device| device.detail.as_str())
                    .unwrap_or("device is still initializing");
                return protocol::ServerMessage::error(
                    id,
                    protocol::ErrorCode::DeviceUnavailable,
                    format!("device `{device_id}` is present but not ready: {detail}"),
                );
            }
        }

        let command = request.command;
        match self.execute_command(command.clone()) {
            Ok(data) => {
                if let Err(error) = self.remember_command(&command, &data) {
                    return protocol::ServerMessage::error(
                        id,
                        protocol::ErrorCode::PersistenceFailed,
                        format!(
                            "the setting was applied and verified, but could not be persisted: {error:#}"
                        ),
                    );
                }
                protocol::ServerMessage::success(id, data)
            }
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
            RequestCommand::Shutdown => Ok(protocol::ResponseData::Acknowledged),
            RequestCommand::ListDevices => Ok(protocol::ResponseData::Devices {
                devices: self
                    .manager
                    .devices()
                    .iter()
                    .map(|device| {
                        let active = self.active.get(&device.id);
                        let unavailable = self.unavailable.get(&device.id);
                        let availability = if active.is_some() {
                            protocol::DeviceAvailability::Ready
                        } else if let Some(unavailable) = unavailable {
                            unavailable.availability()
                        } else {
                            protocol::DeviceAvailability::Initializing
                        };
                        device_summary(
                            device,
                            availability,
                            active
                                .and_then(|mouse| mouse.hardware_id.as_deref())
                                .or_else(|| {
                                    unavailable.and_then(|device| device.hardware_id.as_deref())
                                }),
                            active.and_then(|mouse| mouse.display_name.as_deref()),
                        )
                    })
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

    fn remember_command(
        &mut self,
        command: &protocol::RequestCommand,
        response: &protocol::ResponseData,
    ) -> Result<()> {
        if !command_changes_settings(command) {
            return Ok(());
        }
        let protocol::ResponseData::Device { device } = response else {
            bail!("a setting command returned no device snapshot");
        };
        let hardware_id = device
            .device
            .hardware_id
            .as_deref()
            .context("the device has no persistent HID++ hardware identity")?;
        let preferences = self.settings.device_mut(hardware_id);
        update_preferences(preferences, command, device);

        self.settings.save().with_context(|| {
            format!(
                "failed to save preferences to `{}`",
                self.settings.path().display()
            )
        })
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
        let active = self
            .active
            .get(id)
            .with_context(|| format!("device `{id}` is not ready"))?;
        Ok(protocol::ResponseData::Device {
            device: Box::new(device_state(
                managed,
                &active.mouse,
                active.hardware_id.as_deref(),
                active.display_name.as_deref(),
                &active.capabilities,
                None,
            )?),
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

fn record_unavailable(
    unavailable: &mut BTreeMap<String, UnavailableDevice>,
    device_id: &str,
    reason: protocol::DeviceUnavailableReason,
    detail: String,
    hardware_id: Option<String>,
) -> Option<protocol::AgentEvent> {
    let previous = unavailable.get(device_id);
    let changed = previous.is_none_or(|previous| previous.reason != reason);
    let hardware_id = hardware_id
        .or_else(|| previous.and_then(|previous| previous.hardware_id.as_ref().cloned()));
    unavailable.insert(
        device_id.to_owned(),
        UnavailableDevice {
            reason,
            detail: detail.clone(),
            hardware_id,
        },
    );

    changed.then(|| protocol::AgentEvent::DeviceUnavailable {
        device_id: device_id.to_owned(),
        reason_code: Some(reason),
        reason: detail,
    })
}

fn capture_device_preferences(
    capabilities: &core::DeviceCapabilities,
    settings: &core::SettingsSnapshot,
) -> store::DevicePreferences {
    let control = match settings.configuration_source {
        Some(core::ConfigurationSource::Host) => Some(store::ControlPreference::Host),
        Some(core::ConfigurationSource::Onboard {
            active_profile: Some(profile),
        }) => Some(store::ControlPreference::Onboard { profile }),
        _ => None,
    };
    let host = if matches!(control, Some(store::ControlPreference::Host)) {
        capture_host_preferences(capabilities, settings)
    } else {
        store::HostPreferences::default()
    };
    store::DevicePreferences {
        control,
        host,
        lighting: None,
    }
}

fn update_preferences(
    preferences: &mut store::DevicePreferences,
    command: &protocol::RequestCommand,
    device: &protocol::DeviceState,
) {
    match command {
        protocol::RequestCommand::SetDpi { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.dpi = device.settings.dpi.map(|dpi| dpi.current_x);
        }
        protocol::RequestCommand::SetPollingRate { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.polling_rate =
                polling_preference_from_protocol(device.settings.polling_rate);
        }
        protocol::RequestCommand::SetLiftOffDistance { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.lift_off_distance =
                device.settings.dpi.and_then(|dpi| dpi.lift_off_distance);
        }
        protocol::RequestCommand::SetSurfaceMode { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.surface_mode = device.settings.surface_mode;
        }
        protocol::RequestCommand::SetOperatingMode { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.operating_mode = device.settings.operating_mode;
        }
        protocol::RequestCommand::SetBunnyHopping { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.bunny_hopping =
                device
                    .settings
                    .bunny_hopping
                    .map(|state| store::BunnyHoppingPreference {
                        enabled: state.enabled,
                        timeout_ms: state.timeout_ms,
                    });
        }
        protocol::RequestCommand::SetLighting { zone, effect, .. } => {
            preferences.lighting = Some(store::LightingPreference::Software {
                zone: *zone,
                effect: *effect,
            });
        }
        protocol::RequestCommand::UseFirmwareLighting { .. } => {
            preferences.lighting = Some(store::LightingPreference::Firmware);
        }
        protocol::RequestCommand::UseHostSettings { .. } => {
            preferences.control = Some(store::ControlPreference::Host);
            preferences.host = capture_protocol_host_preferences(device);
        }
        protocol::RequestCommand::UseOnboardProfile { profile, .. } => {
            preferences.control = Some(store::ControlPreference::Onboard { profile: *profile });
        }
        protocol::RequestCommand::Ping
        | protocol::RequestCommand::Shutdown
        | protocol::RequestCommand::ListDevices
        | protocol::RequestCommand::GetDevice { .. }
        | protocol::RequestCommand::Subscribe => {}
    }
}

fn sync_control_preference(
    preferences: &mut store::DevicePreferences,
    settings: &protocol::SettingsState,
) {
    match settings.configuration_source {
        Some(protocol::ConfigurationSource::Host) => {
            preferences.control = Some(store::ControlPreference::Host);
        }
        Some(protocol::ConfigurationSource::Onboard {
            active_profile: Some(profile),
        }) => {
            preferences.control = Some(store::ControlPreference::Onboard { profile });
        }
        _ => {}
    }
}

fn polling_preference_from_protocol(
    polling: protocol::PollingRateState,
) -> Option<store::PollingPreference> {
    match polling {
        protocol::PollingRateState::Unsupported => None,
        protocol::PollingRateState::Shared { hz } => Some(store::PollingPreference::Shared { hz }),
        protocol::PollingRateState::PerConnection {
            wired_hz,
            wireless_hz,
        } => Some(store::PollingPreference::PerConnection {
            wired_hz,
            wireless_hz,
        }),
    }
}

fn capture_protocol_host_preferences(device: &protocol::DeviceState) -> store::HostPreferences {
    store::HostPreferences {
        dpi: device
            .capabilities
            .supported_dpi
            .as_ref()
            .and_then(|_| device.settings.dpi.map(|dpi| dpi.current_x)),
        polling_rate: polling_preference_from_protocol(device.settings.polling_rate),
        lift_off_distance: device
            .capabilities
            .lift_off_distance
            .then(|| device.settings.dpi.and_then(|dpi| dpi.lift_off_distance))
            .flatten(),
        surface_mode: device
            .capabilities
            .surface_mode
            .then_some(device.settings.surface_mode)
            .flatten(),
        operating_mode: device
            .capabilities
            .operating_mode_switch
            .then_some(device.settings.operating_mode)
            .flatten(),
        bunny_hopping: device
            .capabilities
            .bunny_hopping
            .then(|| {
                device
                    .settings
                    .bunny_hopping
                    .map(|state| store::BunnyHoppingPreference {
                        enabled: state.enabled,
                        timeout_ms: state.timeout_ms,
                    })
            })
            .flatten(),
    }
}

fn capture_host_preferences(
    capabilities: &core::DeviceCapabilities,
    settings: &core::SettingsSnapshot,
) -> store::HostPreferences {
    let mode = settings.mode_status;
    store::HostPreferences {
        dpi: capabilities
            .supported_dpi
            .as_ref()
            .and_then(|_| settings.dpi.map(|dpi| dpi.current_x)),
        polling_rate: match settings.polling_rate {
            core::PollingRateSettings::Unsupported => None,
            core::PollingRateSettings::Shared { hz } => {
                Some(store::PollingPreference::Shared { hz })
            }
            core::PollingRateSettings::PerConnection {
                wired_hz,
                wireless_hz,
            } => Some(store::PollingPreference::PerConnection {
                wired_hz,
                wireless_hz,
            }),
        },
        lift_off_distance: capabilities
            .lift_off_distance
            .then(|| settings.dpi.and_then(|dpi| dpi.lod).map(lod_from_core))
            .flatten(),
        surface_mode: capabilities
            .surface_mode
            .then(|| {
                mode.and_then(|mode| mode.surface_mode)
                    .map(surface_from_core)
            })
            .flatten(),
        operating_mode: capabilities
            .operating_mode_switch
            .then(|| mode.map(|mode| operating_from_core(mode.operating_mode())))
            .flatten(),
        bunny_hopping: capabilities
            .bunny_hopping
            .then(|| {
                settings
                    .bunny_hopping
                    .map(|bhop| store::BunnyHoppingPreference {
                        enabled: bhop.enabled,
                        timeout_ms: bhop.timeout_ms,
                    })
            })
            .flatten(),
    }
}

fn restore_device_preferences(
    mouse: &core::MouseDevice,
    connection: core::ConnectionType,
    capabilities: &core::DeviceCapabilities,
    initial: &core::SettingsSnapshot,
    preferences: &store::DevicePreferences,
) -> Result<bool> {
    let mut current = initial.clone();
    let restore_host_settings = match preferences.control {
        Some(store::ControlPreference::Host) => {
            if !matches!(
                current.configuration_source,
                Some(core::ConfigurationSource::Host)
            ) {
                mouse.set_host_control()?;
                current = mouse.settings()?;
            }
            true
        }
        Some(store::ControlPreference::Onboard { profile }) => {
            if !matches!(
                current.configuration_source,
                Some(core::ConfigurationSource::Onboard {
                    active_profile: Some(active)
                }) if active == profile
            ) {
                mouse.activate_onboard_profile(profile)?;
            }
            false
        }
        None => false,
    };

    if restore_host_settings {
        restore_host_preferences(mouse, connection, capabilities, &current, &preferences.host)?;
    }

    match &preferences.lighting {
        Some(store::LightingPreference::Firmware) => {
            mouse.release_color_led_control()?;
            Ok(false)
        }
        Some(store::LightingPreference::Software { zone, effect }) => {
            if !capabilities.color_led_effects {
                bail!("stored lighting preference is unsupported by this mouse");
            }
            mouse.set_color_led_effect(*zone, lighting_to_core(*effect))?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn restore_host_preferences(
    mouse: &core::MouseDevice,
    connection: core::ConnectionType,
    capabilities: &core::DeviceCapabilities,
    current: &core::SettingsSnapshot,
    preferences: &store::HostPreferences,
) -> Result<()> {
    if let Some(dpi) = preferences.dpi
        && capabilities.supported_dpi.is_some()
        && current
            .dpi
            .is_none_or(|state| state.current_x != dpi || state.current_y.is_some_and(|y| y != dpi))
    {
        mouse.set_dpi(dpi)?;
    }

    if let Some(polling) = preferences.polling_rate {
        match (polling, current.polling_rate, connection) {
            (
                store::PollingPreference::Shared { hz },
                core::PollingRateSettings::Shared { hz: current },
                connection,
            ) if hz != current => {
                mouse.set_polling_rate(connection, hz)?;
            }
            (
                store::PollingPreference::PerConnection { wired_hz, .. },
                core::PollingRateSettings::PerConnection {
                    wired_hz: current, ..
                },
                core::ConnectionType::Wired,
            ) if wired_hz != current => {
                mouse.set_polling_rate(core::ConnectionType::Wired, wired_hz)?;
            }
            (
                store::PollingPreference::PerConnection { wireless_hz, .. },
                core::PollingRateSettings::PerConnection {
                    wireless_hz: current,
                    ..
                },
                core::ConnectionType::GamingWireless,
            ) if wireless_hz != current => {
                mouse.set_polling_rate(core::ConnectionType::GamingWireless, wireless_hz)?;
            }
            _ => {}
        }
    }

    if let Some(lod) = preferences.lift_off_distance
        && capabilities.lift_off_distance
        && current.dpi.and_then(|dpi| dpi.lod).map(lod_from_core) != Some(lod)
    {
        mouse.set_lift_off_distance(lod_to_core(lod))?;
    }
    if let Some(surface) = preferences.surface_mode
        && capabilities.surface_mode
        && current
            .mode_status
            .and_then(|mode| mode.surface_mode)
            .map(surface_from_core)
            != Some(surface)
    {
        mouse.set_surface_mode(surface_to_core(surface))?;
    }
    if let Some(mode) = preferences.operating_mode
        && capabilities.operating_mode_switch
        && current
            .mode_status
            .map(|state| operating_from_core(state.operating_mode()))
            != Some(mode)
    {
        mouse.set_operating_mode(operating_to_core(mode))?;
    }
    if let Some(bhop) = preferences.bunny_hopping
        && capabilities.bunny_hopping
        && current.bunny_hopping.is_none_or(|state| {
            state.enabled != bhop.enabled || state.timeout_ms != bhop.timeout_ms
        })
    {
        let timeout_ms = if bhop.enabled {
            bhop.timeout_ms
        } else {
            // The core validates the timeout range even when disabling BHOP because
            // firmware still expects a well-formed request.
            100
        };
        mouse.set_bunny_hopping(bhop.enabled, timeout_ms)?;
    }
    Ok(())
}

fn command_device_id(command: &protocol::RequestCommand) -> Option<&str> {
    use protocol::RequestCommand;
    match command {
        RequestCommand::Ping
        | RequestCommand::Shutdown
        | RequestCommand::ListDevices
        | RequestCommand::Subscribe => None,
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
            | protocol::RequestCommand::Shutdown
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
    hardware_id: Option<&str>,
    display_name: Option<&str>,
    capabilities: &core::DeviceCapabilities,
    settings: Option<core::SettingsSnapshot>,
) -> Result<protocol::DeviceState> {
    let settings = settings.map_or_else(|| mouse.settings(), Ok)?;
    Ok(protocol::DeviceState {
        device: device_summary(
            device,
            protocol::DeviceAvailability::Ready,
            hardware_id,
            display_name,
        ),
        capabilities: capabilities_state(capabilities.clone()),
        settings: settings_state(settings),
    })
}

fn device_summary(
    device: &core::ManagedDevice,
    availability: protocol::DeviceAvailability,
    hardware_id: Option<&str>,
    display_name: Option<&str>,
) -> protocol::DeviceSummary {
    let ready = matches!(availability, protocol::DeviceAvailability::Ready);
    protocol::DeviceSummary {
        id: device.id.clone(),
        hardware_id: hardware_id.map(str::to_owned),
        vendor_id: device.vendor_id,
        product_id: device.product_id,
        product_name: device.product_name.clone(),
        display_name: display_name.map(str::to_owned),
        serial_number: device.serial_number.clone(),
        connection: match device.connection {
            core::DeviceConnection::DirectUsb => protocol::DeviceConnection::DirectUsb,
            core::DeviceConnection::Receiver => protocol::DeviceConnection::Receiver,
        },
        device_index: device.device_index,
        availability: Some(availability),
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

fn operating_from_core(value: core::OperatingMode) -> protocol::OperatingMode {
    match value {
        core::OperatingMode::Endurance => protocol::OperatingMode::Endurance,
        core::OperatingMode::Performance => protocol::OperatingMode::Performance,
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

fn print_initial_state(
    device: &core::ManagedDevice,
    hardware_id: Option<&str>,
    settings: &core::SettingsSnapshot,
) {
    println!("  Ready: {}", device.id);
    println!("  Hardware ID: {}", hardware_id.unwrap_or("unavailable"));
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

    #[cfg(windows)]
    if cli.launch_background {
        return startup::launch_background();
    }

    #[cfg(target_os = "macos")]
    if cli.launch_tray {
        startup::launch_tray()?;
    }

    #[cfg(windows)]
    let background_worker = cli.background_worker;

    if let Some(CliCommand::Startup { action }) = cli.command.as_ref() {
        let (label, status) = match action {
            StartupAction::Install => ("installed", startup::install()?),
            StartupAction::Uninstall => ("removed", startup::uninstall()?),
            StartupAction::Status => ("status", startup::status()?),
        };
        startup::print_status(label, &status);
        return Ok(());
    }

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
    let settings_path = cli
        .settings_file
        .map_or_else(store::SettingsStore::default_path, Ok)?;
    let mut agent = Agent::new(
        Duration::from_secs(cli.battery_interval_seconds),
        settings_path,
        !cli.once,
    )?;

    if cli.once {
        println!("Open Hub agent one-shot discovery; no settings will be changed.");
        agent.tick()?;
        return Ok(());
    }

    println!("Settings: {}", agent.settings.path().display());

    let ipc = ipc::start()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    #[cfg(not(windows))]
    install_shutdown_handler(&shutdown)?;
    #[cfg(windows)]
    if !background_worker {
        install_shutdown_handler(&shutdown)?;
    }
    println!(
        "Open Hub agent started; IPC protocol v{} is ready.",
        protocol::PROTOCOL_VERSION
    );
    let mut next_scan = std::time::Instant::now();
    'run: while !shutdown.load(Ordering::Acquire) {
        #[cfg(windows)]
        match startup::take_stop_request() {
            Ok(true) => {
                println!("Per-user startup requested a graceful agent stop.");
                break;
            }
            Ok(false) => {}
            Err(error) => eprintln!("Could not check the startup stop request: {error:#}"),
        }

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
            let shutdown_requested =
                matches!(pending.request.command, protocol::RequestCommand::Shutdown);
            let publish_change = command_changes_settings(&pending.request.command);
            let response = agent.handle_request(pending.request);
            if publish_change {
                if let Some(event) = settings_event_from_response(&response) {
                    ipc.publish(event);
                }
            }
            let _ = pending.reply.send(response);
            if shutdown_requested {
                break 'run;
            }
        }

        let wait = next_scan
            .saturating_duration_since(std::time::Instant::now())
            .min(Duration::from_secs(1));
        if let Some(pending) = ipc.recv_timeout(wait)? {
            let shutdown_requested =
                matches!(pending.request.command, protocol::RequestCommand::Shutdown);
            let publish_change = command_changes_settings(&pending.request.command);
            let response = agent.handle_request(pending.request);
            if publish_change {
                if let Some(event) = settings_event_from_response(&response) {
                    ipc.publish(event);
                }
            }
            let _ = pending.reply.send(response);
            if shutdown_requested {
                break 'run;
            }
        }
    }
    ipc.publish(protocol::AgentEvent::ApplicationShuttingDown);
    agent.shutdown();
    println!("Open Hub agent stopped cleanly.");
    Ok(())
}

fn install_shutdown_handler(shutdown: &Arc<AtomicBool>) -> Result<()> {
    let shutdown_signal = Arc::clone(shutdown);
    ctrlc::set_handler(move || shutdown_signal.store(true, Ordering::Release))
        .context("failed to install the graceful-shutdown handler")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_startup_commands() {
        let cli = Cli::try_parse_from(["open-hub-agent", "startup", "install"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Startup {
                action: StartupAction::Install
            })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn parses_detached_background_worker_flag() {
        let cli = Cli::try_parse_from(["open-hub-agent", "--background-worker"]).unwrap();
        assert!(cli.background_worker);
        assert!(!cli.launch_background);
    }

    fn sample_device_state() -> protocol::DeviceState {
        protocol::DeviceState {
            device: protocol::DeviceSummary {
                id: "session-id".to_owned(),
                hardware_id: Some("046d:unit:1077e69f".to_owned()),
                vendor_id: 0x046d,
                product_id: 0xc54d,
                product_name: Some("USB Receiver".to_owned()),
                display_name: Some("PRO X Superlight 2".to_owned()),
                serial_number: None,
                connection: protocol::DeviceConnection::Receiver,
                device_index: 1,
                availability: Some(protocol::DeviceAvailability::Ready),
                ready: true,
            },
            capabilities: protocol::DeviceCapabilities {
                battery: true,
                supported_dpi: Some(vec![800, 1600]),
                polling_rates: protocol::PollingRateCapabilities::PerConnection {
                    wired_hz: vec![1000],
                    wireless_hz: vec![1000, 2000],
                },
                onboard_profiles: true,
                onboard_profile_description: None,
                lift_off_distance: true,
                surface_mode: true,
                operating_mode_switch: false,
                color_led_effects: false,
                bunny_hopping: true,
                mouse_button_filter: true,
            },
            settings: protocol::SettingsState {
                battery: None,
                dpi: Some(protocol::DpiState {
                    current_x: 800,
                    default_x: 800,
                    current_y: Some(800),
                    default_y: Some(800),
                    lift_off_distance: Some(protocol::LiftOffDistance::High),
                }),
                polling_rate: protocol::PollingRateState::PerConnection {
                    wired_hz: 1000,
                    wireless_hz: 2000,
                },
                configuration_source: Some(protocol::ConfigurationSource::Host),
                operating_mode: Some(protocol::OperatingMode::Endurance),
                surface_mode: Some(protocol::SurfaceMode::Off),
                bunny_hopping: Some(protocol::BunnyHoppingState {
                    enabled: false,
                    timeout_ms: 0,
                }),
            },
        }
    }

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

    #[test]
    fn host_mode_captures_supported_settings_for_restore() {
        let state = sample_device_state();
        let mut preferences = store::DevicePreferences::default();
        update_preferences(
            &mut preferences,
            &protocol::RequestCommand::UseHostSettings {
                device_id: "session-id".to_owned(),
            },
            &state,
        );

        assert_eq!(preferences.control, Some(store::ControlPreference::Host));
        assert_eq!(preferences.host.dpi, Some(800));
        assert_eq!(
            preferences.host.polling_rate,
            Some(store::PollingPreference::PerConnection {
                wired_hz: 1000,
                wireless_hz: 2000,
            })
        );
        assert_eq!(
            preferences.host.lift_off_distance,
            Some(protocol::LiftOffDistance::High)
        );
        assert_eq!(preferences.host.operating_mode, None);
    }

    #[test]
    fn selecting_onboard_profile_preserves_saved_host_preferences() {
        let mut state = sample_device_state();
        let mut preferences = store::DevicePreferences {
            control: Some(store::ControlPreference::Host),
            host: capture_protocol_host_preferences(&state),
            lighting: None,
        };
        let saved_host = preferences.host.clone();
        state.settings.configuration_source = Some(protocol::ConfigurationSource::Onboard {
            active_profile: Some(1),
        });
        update_preferences(
            &mut preferences,
            &protocol::RequestCommand::UseOnboardProfile {
                device_id: "session-id".to_owned(),
                profile: 1,
            },
            &state,
        );

        assert_eq!(
            preferences.control,
            Some(store::ControlPreference::Onboard { profile: 1 })
        );
        assert_eq!(preferences.host, saved_host);
    }

    #[test]
    fn unavailable_transition_is_emitted_once_until_device_recovers() {
        let mut unavailable = BTreeMap::new();
        let first = record_unavailable(
            &mut unavailable,
            "mouse-1",
            protocol::DeviceUnavailableReason::NotResponding,
            "mouse may be switched off".to_owned(),
            Some("hardware-1".to_owned()),
        );
        assert!(matches!(
            first,
            Some(protocol::AgentEvent::DeviceUnavailable {
                reason_code: Some(protocol::DeviceUnavailableReason::NotResponding),
                ..
            })
        ));

        let retry = record_unavailable(
            &mut unavailable,
            "mouse-1",
            protocol::DeviceUnavailableReason::NotResponding,
            "second timeout".to_owned(),
            None,
        );
        assert_eq!(retry, None);
        assert_eq!(unavailable["mouse-1"].detail, "second timeout");
        assert_eq!(
            unavailable["mouse-1"].hardware_id.as_deref(),
            Some("hardware-1")
        );

        // A successful open removes the unavailable marker and emits DeviceReady.
        assert!(unavailable.remove("mouse-1").is_some());
        let after_recovery = record_unavailable(
            &mut unavailable,
            "mouse-1",
            protocol::DeviceUnavailableReason::CommunicationError,
            "new failure after recovery".to_owned(),
            Some("hardware-1".to_owned()),
        );
        assert!(after_recovery.is_some());
    }
}
