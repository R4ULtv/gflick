mod ipc;
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
use clap::Parser;
use gflick_core as core;
use gflick_protocol as protocol;

/// Bounds what a client can write into the settings file as a display name.
const MAX_NICKNAME_CHARS: usize = 64;

#[derive(Debug, Parser)]
#[command(name = "gflick-agent", about = "Low-overhead GFlick mouse agent")]
struct Cli {
    /// Seconds between USB device discovery passes.
    #[arg(long, default_value_t = 5)]
    scan_interval_seconds: u64,

    /// Seconds between battery queries, including charging status, for each open mouse.
    #[arg(long, default_value_t = 30)]
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

    /// Internal detached Windows agent process.
    #[cfg(windows)]
    #[arg(long, hide = true)]
    background_worker: bool,
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

/// Stored metadata keyed by persistent HID++ identity, even without a live session.
struct ResolvedDeviceMetadata<'a> {
    hardware_id: Option<String>,
    preferences: Option<&'a store::DevicePreferences>,
}

impl UnavailableDevice {
    fn availability(&self) -> protocol::DeviceAvailability {
        protocol::DeviceAvailability::Unavailable {
            reason: self.reason,
            detail: self.detail.clone(),
        }
    }
}

fn reschedule_battery_check(
    deadline: std::time::Instant,
    previous: Duration,
    next: Duration,
) -> std::time::Instant {
    deadline
        .checked_sub(previous)
        .unwrap_or_else(std::time::Instant::now)
        + next
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

    fn set_battery_interval(&mut self, interval: Duration) {
        if interval == self.battery_interval {
            return;
        }
        for active in self.active.values_mut() {
            active.battery_check_after = reschedule_battery_check(
                active.battery_check_after,
                self.battery_interval,
                interval,
            );
        }
        self.battery_interval = interval;
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

        if self.restore_preferences {
            self.remember_identity(hardware_id.as_deref(), display_name.as_deref(), device);
        }

        print_initial_state(device, hardware_id.as_deref(), &settings);
        let state = device_state(
            device,
            &mouse,
            hardware_id.as_deref(),
            display_name.as_deref(),
            &capabilities,
            Some(settings.clone()),
            hardware_id
                .as_deref()
                .and_then(|id| self.settings.device(id)),
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
            let metadata = self.resolve_device_metadata(device, None);
            events.push(protocol::AgentEvent::DeviceConnected {
                device: device_summary(
                    device,
                    protocol::DeviceAvailability::Initializing,
                    metadata.hardware_id.as_deref(),
                    None,
                    metadata.preferences,
                ),
            });
        }

        // Consume buffered receiver link events before retrying, without waking mice.
        let mut link_disconnected = Vec::new();
        for (id, active) in &self.active {
            match active.mouse.poll_link_status() {
                Ok(Some(core::DeviceLinkStatus::Disconnected)) => {
                    link_disconnected.push((id.clone(), active.hardware_id.clone()));
                }
                Ok(Some(core::DeviceLinkStatus::Connected) | None) => {}
                Err(error) => {
                    eprintln!("Could not read buffered link status [{id}]: {error:#}");
                }
            }
        }
        for (id, hardware_id) in &link_disconnected {
            self.active.remove(id);
            self.mark_unavailable(
                id,
                protocol::DeviceUnavailableReason::NotResponding,
                "wireless link disconnected; mouse may be asleep, switched off, or out of range"
                    .to_owned(),
                hardware_id.clone(),
                &mut events,
            );
        }

        // A USB interface may remain present while a mouse is asleep or switched
        // off. Keep retrying, but emit unavailable only when its reason changes.
        let candidates = self.manager.devices().to_vec();
        for device in candidates {
            if self.active.contains_key(&device.id)
                || link_disconnected.iter().any(|(id, _)| id == &device.id)
            {
                continue;
            }

            let session_hardware_id = self
                .unavailable
                .get(&device.id)
                .and_then(|unavailable| unavailable.hardware_id.as_deref());
            let resolved_hardware_id = self
                .resolve_device_metadata(&device, session_hardware_id)
                .hardware_id;

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
                            resolved_hardware_id,
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
                        resolved_hardware_id,
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
            RequestCommand::ListSavedDevices => Ok(protocol::ResponseData::Devices {
                devices: self.settings.saved_devices(),
            }),
            RequestCommand::GetAppPreferences => Ok(protocol::ResponseData::AppPreferences {
                preferences: self.settings.app_preferences(),
            }),
            RequestCommand::SetAppPreferences { preferences } => {
                self.settings.set_app_preferences(preferences)?;
                Ok(protocol::ResponseData::Acknowledged)
            }
            RequestCommand::Ping => Ok(protocol::ResponseData::Pong),
            RequestCommand::Shutdown => Ok(protocol::ResponseData::Acknowledged),
            RequestCommand::ListDevices => Ok(protocol::ResponseData::Devices {
                devices: self.device_summaries(),
            }),
            RequestCommand::GetDevice { device_id } => self.device_response(&device_id),
            RequestCommand::Subscribe | RequestCommand::SubscribeSettings => {
                Ok(protocol::ResponseData::Subscribed)
            }
            RequestCommand::SetDeviceColor { device_id, color } => {
                self.set_color(&device_id, color)
            }
            RequestCommand::SetDeviceNickname {
                device_id,
                nickname,
            } => self.set_nickname(&device_id, nickname),
            RequestCommand::ReorderDevices { hardware_ids } => self.reorder(&hardware_ids),
            RequestCommand::SetOnboardDpiStage { device_id, index } => {
                self.mouse(&device_id)?
                    .set_current_onboard_dpi_stage(index)?;
                self.device_response(&device_id)
            }
            RequestCommand::SetDpiAxes { device_id, x, y } => {
                self.mouse(&device_id)?.set_dpi_axes(x, y)?;
                self.device_response(&device_id)
            }
            RequestCommand::SetMouseButtonMapping { device_id, mapping } => {
                self.mouse(&device_id)?.set_mouse_button_filter(&mapping)?;
                self.device_response(&device_id)
            }
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

    /// Stores a host-side display name. Nothing is sent to the device, so this is
    /// allowed while the mouse is still initializing.
    fn set_nickname(
        &mut self,
        device_id: &str,
        nickname: Option<String>,
    ) -> Result<protocol::ResponseData> {
        let hardware_id = self
            .hardware_id(device_id)
            .context("the device has no persistent HID++ hardware identity yet")?;

        update_nickname(&mut self.settings, &hardware_id, nickname)?;
        self.save_settings()?;
        Ok(protocol::ResponseData::Acknowledged)
    }

    fn set_color(
        &mut self,
        device_id: &str,
        color: protocol::DeviceColor,
    ) -> Result<protocol::ResponseData> {
        let device = self
            .device_summaries()
            .into_iter()
            .find(|d| d.id == device_id)
            .context("device not found")?;
        if !device.available_colors().contains(&color) {
            bail!("the selected color is not available for this mouse model");
        }
        let hardware_id = device
            .hardware_id
            .context("the device has no persistent HID++ hardware identity yet")?;
        let previous = self.settings.device_mut(&hardware_id).color;
        self.settings.device_mut(&hardware_id).color = color;
        if let Err(error) = self.save_settings() {
            self.settings.device_mut(&hardware_id).color = previous;
            return Err(error);
        }
        Ok(protocol::ResponseData::Acknowledged)
    }

    /// Assigns list positions by hardware identity. Unlisted devices keep `None`
    /// and sort after the ordered ones.
    fn reorder(&mut self, hardware_ids: &[String]) -> Result<protocol::ResponseData> {
        self.settings.replace_device_order(hardware_ids)?;
        self.save_settings()?;
        Ok(protocol::ResponseData::Acknowledged)
    }

    /// Caches model and USB identity for matching devices across reconnects.
    /// Writes only on change, and save failures do not fail device setup.
    fn remember_identity(
        &mut self,
        hardware_id: Option<&str>,
        display_name: Option<&str>,
        device: &core::ManagedDevice,
    ) {
        let Some(hardware_id) = hardware_id else {
            return;
        };
        let identity = usb_identity(device);
        let preferences = self.settings.device_mut(hardware_id);
        let name_matches =
            display_name.is_none() || preferences.cached_model_name.as_deref() == display_name;
        if name_matches && preferences.usb_identity.as_ref() == Some(&identity) {
            return;
        }
        if let Some(display_name) = display_name {
            preferences.cached_model_name = Some(display_name.to_owned());
        }
        preferences.usb_identity = Some(identity);
        if let Err(error) = self.save_settings() {
            eprintln!("Could not save the device identity for {hardware_id}: {error:#}");
        }
    }

    fn hardware_id(&self, device_id: &str) -> Option<String> {
        let device = self.manager.device(device_id)?;
        let session_hardware_id = self
            .active
            .get(device_id)
            .and_then(|mouse| mouse.hardware_id.as_deref())
            .or_else(|| {
                self.unavailable
                    .get(device_id)
                    .and_then(|device| device.hardware_id.as_deref())
            });
        self.resolve_device_metadata(device, session_hardware_id)
            .hardware_id
    }

    /// Resolves metadata from a live identity, falling back to a canonical USB match.
    fn resolve_device_metadata(
        &self,
        device: &core::ManagedDevice,
        session_hardware_id: Option<&str>,
    ) -> ResolvedDeviceMetadata<'_> {
        resolve_device_metadata(&self.settings, &usb_identity(device), session_hardware_id)
    }

    fn save_settings(&self) -> Result<()> {
        self.settings.save().with_context(|| {
            format!(
                "failed to save preferences to `{}`",
                self.settings.path().display()
            )
        })
    }

    /// Builds the authoritative ordered host-metadata snapshot used by both
    /// `list_devices` and metadata-change events.
    fn device_summaries(&self) -> Vec<protocol::DeviceSummary> {
        let mut devices: Vec<protocol::DeviceSummary> = self
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
                let session_hardware_id = active
                    .and_then(|mouse| mouse.hardware_id.as_deref())
                    .or_else(|| unavailable.and_then(|device| device.hardware_id.as_deref()));
                let metadata = self.resolve_device_metadata(device, session_hardware_id);
                device_summary(
                    device,
                    availability,
                    metadata.hardware_id.as_deref(),
                    active.and_then(|mouse| mouse.display_name.as_deref()),
                    metadata.preferences,
                )
            })
            .collect();
        sort_by_user_order(&mut devices);
        devices
    }

    fn publication_events_from_response(
        &self,
        command: &protocol::RequestCommand,
        response: &protocol::ServerMessage,
    ) -> Vec<protocol::AgentEvent> {
        response_publication_events(command, response, || {
            device_metadata_event(self.device_summaries())
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
                active
                    .hardware_id
                    .as_deref()
                    .and_then(|id| self.settings.device(id)),
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

/// Resolves identity consistently for summaries, lifecycle records, and offline commands.
/// Live identities win; USB fallback requires exactly one stored match.
fn resolve_device_metadata<'a>(
    settings: &'a store::SettingsStore,
    usb_identity: &store::UsbIdentity,
    session_hardware_id: Option<&str>,
) -> ResolvedDeviceMetadata<'a> {
    match session_hardware_id {
        Some(hardware_id) => ResolvedDeviceMetadata {
            hardware_id: Some(hardware_id.to_owned()),
            preferences: settings.device(hardware_id),
        },
        None => match settings.resolve_device_by_usb_identity(usb_identity) {
            Some((hardware_id, preferences)) => ResolvedDeviceMetadata {
                hardware_id: Some(hardware_id.to_owned()),
                preferences: Some(preferences),
            },
            None => ResolvedDeviceMetadata {
                hardware_id: None,
                preferences: None,
            },
        },
    }
}

fn update_nickname(
    settings: &mut store::SettingsStore,
    hardware_id: &str,
    nickname: Option<String>,
) -> Result<()> {
    let nickname = match nickname {
        Some(name) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.chars().count() > MAX_NICKNAME_CHARS {
                bail!("nickname must be at most {MAX_NICKNAME_CHARS} characters");
            } else {
                Some(trimmed.to_owned())
            }
        }
        None => None,
    };
    settings.device_mut(hardware_id).nickname = nickname;
    Ok(())
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
        onboard_dpi_stage: None,
        control,
        host,
        lighting: None,
        nickname: None,
        color: Default::default(),
        sort_order: None,
        // Both are recorded separately once the mouse reports its name.
        cached_model_name: None,
        usb_identity: None,
    }
}

fn update_preferences(
    preferences: &mut store::DevicePreferences,
    command: &protocol::RequestCommand,
    device: &protocol::DeviceState,
) {
    match command {
        protocol::RequestCommand::SetDpi { .. } | protocol::RequestCommand::SetDpiAxes { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.dpi = device.settings.dpi.map(|dpi| dpi.current_x);
            preferences.host.dpi_y = device.settings.dpi.and_then(|dpi| dpi.current_y);
        }
        protocol::RequestCommand::SetOnboardDpiStage { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.onboard_dpi_stage = device.settings.onboard_dpi_stage;
        }
        protocol::RequestCommand::SetMouseButtonMapping { .. } => {
            sync_control_preference(preferences, &device.settings);
            preferences.host.button_mapping = device.settings.mouse_button_mapping.clone();
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
            preferences.onboard_dpi_stage = device.settings.onboard_dpi_stage;
        }
        protocol::RequestCommand::Ping
        | protocol::RequestCommand::Shutdown
        | protocol::RequestCommand::ListDevices
        | protocol::RequestCommand::ListSavedDevices
        | protocol::RequestCommand::GetAppPreferences
        | protocol::RequestCommand::SetAppPreferences { .. }
        | protocol::RequestCommand::GetDevice { .. }
        | protocol::RequestCommand::Subscribe
        | protocol::RequestCommand::SubscribeSettings
        | protocol::RequestCommand::SetDeviceColor { .. }
        | protocol::RequestCommand::SetDeviceNickname { .. }
        | protocol::RequestCommand::ReorderDevices { .. } => {}
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
        dpi_y: device.settings.dpi.and_then(|dpi| dpi.current_y),
        button_mapping: device.settings.mouse_button_mapping.clone(),
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
        dpi_y: settings.dpi.and_then(|dpi| dpi.current_y),
        button_mapping: None,
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
            if let Some(stage) = preferences.onboard_dpi_stage {
                if mouse.current_onboard_dpi_stage()? != Some(stage) {
                    mouse.set_current_onboard_dpi_stage(stage)?;
                }
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
    if let Some(mapping) = &preferences.button_mapping {
        if !capabilities.mouse_button_filter {
            bail!("stored button mapping is unsupported");
        }
        if mouse
            .mouse_button_filter()?
            .is_none_or(|info| &info.mapping != mapping)
        {
            mouse.set_mouse_button_filter(mapping)?;
        }
    }
    if let Some(dpi) = preferences.dpi
        && capabilities.supported_dpi.is_some()
        && current.dpi.is_none_or(|state| {
            state.current_x != dpi
                || state
                    .current_y
                    .is_some_and(|y| y != preferences.dpi_y.unwrap_or(dpi))
        })
    {
        if current.dpi.is_some_and(|d| d.current_y.is_some()) {
            mouse.set_dpi_axes(dpi, preferences.dpi_y.unwrap_or(dpi))?;
        } else {
            mouse.set_dpi(dpi)?;
        }
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
        | RequestCommand::ListSavedDevices
        | RequestCommand::GetAppPreferences
        | RequestCommand::SetAppPreferences { .. }
        | RequestCommand::Subscribe
        | RequestCommand::SubscribeSettings
        // Host-side metadata: deliberately exempt from the device-ready guard so a
        // device can be renamed while it is still initializing.
        | RequestCommand::SetDeviceColor { .. }
        | RequestCommand::SetDeviceNickname { .. }
        | RequestCommand::ReorderDevices { .. } => None,
        RequestCommand::GetDevice { device_id }
        | RequestCommand::SetDpi { device_id, .. }
        | RequestCommand::SetDpiAxes { device_id, .. }
        | RequestCommand::SetOnboardDpiStage { device_id, .. }
        | RequestCommand::SetMouseButtonMapping { device_id, .. }
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
        | protocol::RequestCommand::ListSavedDevices
        | protocol::RequestCommand::GetAppPreferences
        | protocol::RequestCommand::SetAppPreferences { .. }
            | protocol::RequestCommand::GetDevice { .. }
            | protocol::RequestCommand::Subscribe
        | protocol::RequestCommand::SubscribeSettings
            // These persist host-side metadata themselves and return no snapshot.
            | protocol::RequestCommand::SetDeviceColor { .. }
            | protocol::RequestCommand::SetDeviceNickname { .. }
            | protocol::RequestCommand::ReorderDevices { .. }
    )
}

fn command_changes_metadata(command: &protocol::RequestCommand) -> bool {
    matches!(
        command,
        protocol::RequestCommand::SetDeviceColor { .. }
            | protocol::RequestCommand::SetDeviceNickname { .. }
            | protocol::RequestCommand::ReorderDevices { .. }
    )
}

fn metadata_change_acknowledged(
    command: &protocol::RequestCommand,
    response: &protocol::ServerMessage,
) -> bool {
    command_changes_metadata(command)
        && matches!(
            response,
            protocol::ServerMessage::Response {
                result: protocol::ResponseResult::Success {
                    data: protocol::ResponseData::Acknowledged
                },
                ..
            }
        )
}

fn device_metadata_event(devices: Vec<protocol::DeviceSummary>) -> protocol::AgentEvent {
    protocol::AgentEvent::DeviceMetadataChanged { devices }
}

/// Maps a completed command to one publication category, building metadata lazily.
fn response_publication_events(
    command: &protocol::RequestCommand,
    response: &protocol::ServerMessage,
    build_metadata_event: impl FnOnce() -> protocol::AgentEvent,
) -> Vec<protocol::AgentEvent> {
    if command_changes_settings(command) {
        return settings_event_from_response(response).into_iter().collect();
    }
    if metadata_change_acknowledged(command, response) {
        return vec![build_metadata_event()];
    }
    Vec::new()
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
    preferences: Option<&store::DevicePreferences>,
) -> Result<protocol::DeviceState> {
    let settings = settings.map_or_else(|| mouse.settings(), Ok)?;
    let mut settings = settings_state(settings);
    if matches!(
        settings.configuration_source,
        Some(protocol::ConfigurationSource::Onboard { .. })
    ) {
        settings.onboard_dpi_stage = mouse.current_onboard_dpi_stage()?;
    }
    if capabilities.mouse_button_filter {
        settings.mouse_button_mapping = mouse.mouse_button_filter()?.map(|info| info.mapping);
    }
    Ok(protocol::DeviceState {
        device: device_summary(
            device,
            protocol::DeviceAvailability::Ready,
            hardware_id,
            display_name,
            preferences,
        ),
        capabilities: capabilities_state(capabilities.clone()),
        settings,
    })
}

/// Orders devices by their stored position. Devices without one keep discovery
/// order behind the ordered ones, which is why this uses a stable sort.
fn sort_by_user_order(devices: &mut [protocol::DeviceSummary]) {
    devices.sort_by(|a, b| match (a.sort_order, b.sort_order) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

fn usb_identity(device: &core::ManagedDevice) -> store::UsbIdentity {
    store::UsbIdentity::new(
        device.vendor_id,
        device.product_id,
        device.device_index,
        device.serial_number.clone(),
    )
}

fn device_summary(
    device: &core::ManagedDevice,
    availability: protocol::DeviceAvailability,
    hardware_id: Option<&str>,
    display_name: Option<&str>,
    preferences: Option<&store::DevicePreferences>,
) -> protocol::DeviceSummary {
    let ready = matches!(availability, protocol::DeviceAvailability::Ready);
    protocol::DeviceSummary {
        id: device.id.clone(),
        hardware_id: hardware_id.map(str::to_owned),
        vendor_id: device.vendor_id,
        product_id: device.product_id,
        product_name: device.product_name.clone(),
        // A live name wins; the cached one keeps a sleeping mouse identifiable
        // instead of showing the receiver's USB product name.
        display_name: display_name
            .map(str::to_owned)
            .or_else(|| preferences.and_then(|preferences| preferences.cached_model_name.clone())),
        serial_number: device.serial_number.clone(),
        nickname: preferences.and_then(|p| p.nickname.clone()),
        color: preferences.map(|p| p.color).unwrap_or_default(),
        sort_order: preferences.and_then(|p| p.sort_order),
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
        onboard_dpi_stage: None,
        mouse_button_mapping: None,
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
    let background_worker = cli.background_worker;

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
        println!("GFlick agent one-shot discovery; no settings will be changed.");
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
        "GFlick agent started; IPC protocol v{} is ready.",
        protocol::PROTOCOL_VERSION
    );
    let background_battery_interval = agent.battery_interval;
    let mut next_scan = std::time::Instant::now();
    'run: while !shutdown.load(Ordering::Acquire) {
        let now = std::time::Instant::now();
        agent.set_battery_interval(ipc.battery_interval(background_battery_interval));
        next_scan = next_scan.min(now + agent.battery_interval);
        if now >= next_scan {
            match agent.tick() {
                Ok(events) => {
                    for event in events {
                        ipc.publish(event);
                    }
                }
                Err(error) => eprintln!("Discovery pass failed: {error:#}"),
            }
            next_scan = std::time::Instant::now() + scan_interval.min(agent.battery_interval);
        }

        while let Some(pending) = ipc.try_recv()? {
            if handle_pending_request(&mut agent, &ipc, pending) {
                break 'run;
            }
        }

        let wait = next_scan
            .saturating_duration_since(std::time::Instant::now())
            .min(Duration::from_secs(1));
        if let Some(pending) = ipc.recv_timeout(wait)? {
            if handle_pending_request(&mut agent, &ipc, pending) {
                break 'run;
            }
        }
    }
    ipc.publish(protocol::AgentEvent::ApplicationShuttingDown);
    agent.shutdown();
    println!("GFlick agent stopped cleanly.");
    Ok(())
}

fn handle_pending_request(
    agent: &mut Agent,
    ipc: &ipc::IpcHandle,
    pending: ipc::PendingRequest,
) -> bool {
    let shutdown_requested = matches!(pending.request.command, protocol::RequestCommand::Shutdown);
    let command = pending.request.command.clone();
    let response = agent.handle_request(pending.request);
    let events = agent.publication_events_from_response(&command, &response);

    let _ = pending.reply.send(response);
    for event in events {
        ipc.publish(event);
    }
    shutdown_requested
}

fn install_shutdown_handler(shutdown: &Arc<AtomicBool>) -> Result<()> {
    let shutdown_signal = Arc::clone(shutdown);
    ctrlc::set_handler(move || shutdown_signal.store(true, Ordering::Release))
        .context("failed to install the graceful-shutdown handler")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_store() -> (tempfile::TempDir, store::SettingsStore) {
        let directory = tempfile::tempdir().unwrap();
        let settings = store::SettingsStore::load(directory.path().join("settings.json")).unwrap();
        (directory, settings)
    }

    fn test_usb_identity(serial_number: Option<&str>) -> store::UsbIdentity {
        store::UsbIdentity::new(0x046d, 0xc54d, 1, serial_number.map(str::to_owned))
    }

    fn remember_test_device(
        settings: &mut store::SettingsStore,
        hardware_id: &str,
        identity: store::UsbIdentity,
    ) {
        let preferences = settings.device_mut(hardware_id);
        preferences.usb_identity = Some(identity);
        preferences.nickname = Some("Desk mouse".to_owned());
        preferences.sort_order = Some(2);
        preferences.cached_model_name = Some("PRO X Superlight 2".to_owned());
    }

    #[test]
    fn changing_battery_cadence_reschedules_from_the_last_check() {
        let last = std::time::Instant::now();
        let background = Duration::from_secs(30);
        let foreground = Duration::from_secs(5);
        let fast = reschedule_battery_check(last + background, background, foreground);
        assert_eq!(fast, last + foreground);
        assert_eq!(
            reschedule_battery_check(fast, foreground, background),
            last + background
        );
    }

    #[test]
    fn rejects_removed_startup_commands() {
        assert!(Cli::try_parse_from(["gflick-agent", "startup", "install"]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn parses_detached_background_worker_flag() {
        let cli = Cli::try_parse_from(["gflick-agent", "--background-worker"]).unwrap();
        assert!(cli.background_worker);
    }

    fn ordered_summary(id: &str, sort_order: Option<u32>) -> protocol::DeviceSummary {
        protocol::DeviceSummary {
            id: id.to_owned(),
            hardware_id: Some(id.to_owned()),
            vendor_id: 0x046d,
            product_id: 0xc54d,
            product_name: None,
            display_name: None,
            serial_number: None,
            nickname: None,
            color: Default::default(),
            sort_order,
            connection: protocol::DeviceConnection::Receiver,
            device_index: 1,
            availability: Some(protocol::DeviceAvailability::Ready),
            ready: true,
        }
    }

    #[test]
    fn user_order_precedes_unordered_devices_in_discovery_order() {
        let mut devices = vec![
            ordered_summary("unordered-first", None),
            ordered_summary("second", Some(1)),
            ordered_summary("unordered-second", None),
            ordered_summary("first", Some(0)),
        ];
        sort_by_user_order(&mut devices);

        let ids: Vec<&str> = devices.iter().map(|device| device.id.as_str()).collect();
        assert_eq!(
            ids,
            ["first", "second", "unordered-first", "unordered-second"]
        );
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
                nickname: None,
                color: Default::default(),
                sort_order: None,
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
                onboard_dpi_stage: None,
                mouse_button_mapping: None,
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
    fn color_change_is_metadata_and_survives_hardware_setting_updates() {
        let command = protocol::RequestCommand::SetDeviceColor {
            device_id: "mouse-1".into(),
            color: protocol::DeviceColor::Cyan,
        };
        assert_eq!(command_device_id(&command), None);
        assert!(!command_changes_settings(&command));
        assert!(command_changes_metadata(&command));
        let mut preferences = store::DevicePreferences {
            color: protocol::DeviceColor::Cyan,
            ..Default::default()
        };
        let state = sample_device_state();
        update_preferences(
            &mut preferences,
            &protocol::RequestCommand::SetDpi {
                device_id: "mouse-1".into(),
                dpi: 1600,
            },
            &state,
        );
        assert_eq!(preferences.color, protocol::DeviceColor::Cyan);
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
    fn publication_seam_emits_exactly_one_metadata_event_for_successful_host_changes() {
        let rename = protocol::RequestCommand::SetDeviceNickname {
            device_id: "mouse-1".to_owned(),
            nickname: Some("Desk mouse".to_owned()),
        };
        let reorder = protocol::RequestCommand::ReorderDevices {
            hardware_ids: vec!["hardware-1".to_owned()],
        };
        let acknowledged =
            protocol::ServerMessage::success(1, protocol::ResponseData::Acknowledged);

        for command in [&rename, &reorder] {
            let builder_calls = std::cell::Cell::new(0);
            let events = response_publication_events(command, &acknowledged, || {
                builder_calls.set(builder_calls.get() + 1);
                device_metadata_event(vec![])
            });
            assert_eq!(builder_calls.get(), 1);
            assert_eq!(events, vec![device_metadata_event(vec![])]);
        }
    }

    #[test]
    fn publication_seam_skips_metadata_for_failed_and_non_mutating_commands() {
        let rename = protocol::RequestCommand::SetDeviceNickname {
            device_id: "mouse-1".to_owned(),
            nickname: Some("Desk mouse".to_owned()),
        };
        let reorder = protocol::RequestCommand::ReorderDevices {
            hardware_ids: vec!["hardware-1".to_owned()],
        };
        let failures = [
            protocol::ServerMessage::error(
                1,
                protocol::ErrorCode::PersistenceFailed,
                "could not save preferences",
            ),
            protocol::ServerMessage::error(
                1,
                protocol::ErrorCode::OperationFailed,
                "duplicate hardware ID",
            ),
        ];

        for (command, response) in [
            (&rename, &failures[0]),
            (&reorder, &failures[1]),
            (
                &protocol::RequestCommand::ListDevices,
                &protocol::ServerMessage::success(1, protocol::ResponseData::Acknowledged),
            ),
        ] {
            let builder_calls = std::cell::Cell::new(0);
            let events = response_publication_events(command, response, || {
                builder_calls.set(builder_calls.get() + 1);
                device_metadata_event(vec![])
            });
            assert!(events.is_empty());
            assert_eq!(builder_calls.get(), 0);
        }
    }

    #[test]
    fn publication_seam_keeps_hid_changes_as_one_settings_event() {
        let command = protocol::RequestCommand::SetDpi {
            device_id: "mouse-1".to_owned(),
            dpi: 800,
        };
        let state = sample_device_state();
        let response = protocol::ServerMessage::success(
            1,
            protocol::ResponseData::Device {
                device: Box::new(state.clone()),
            },
        );
        let builder_calls = std::cell::Cell::new(0);
        let events = response_publication_events(&command, &response, || {
            builder_calls.set(builder_calls.get() + 1);
            device_metadata_event(vec![])
        });

        assert_eq!(builder_calls.get(), 0);
        assert_eq!(
            events,
            vec![protocol::AgentEvent::SettingsChanged {
                device: Box::new(state)
            }]
        );
    }

    #[test]
    fn list_and_metadata_event_use_the_same_agent_summary_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = Agent::new(
            Duration::from_secs(1),
            directory.path().join("settings.json"),
            false,
        )
        .unwrap();
        let list_response = agent
            .execute_command(protocol::RequestCommand::ListDevices)
            .unwrap();
        let acknowledged =
            protocol::ServerMessage::success(1, protocol::ResponseData::Acknowledged);
        let events = agent.publication_events_from_response(
            &protocol::RequestCommand::SetDeviceNickname {
                device_id: "mouse-1".to_owned(),
                nickname: Some("Desk mouse".to_owned()),
            },
            &acknowledged,
        );

        let protocol::ResponseData::Devices { devices } = list_response else {
            panic!("expected a device list response");
        };
        let [
            protocol::AgentEvent::DeviceMetadataChanged {
                devices: event_devices,
            },
        ] = events.as_slice()
        else {
            panic!("expected a metadata event");
        };
        assert_eq!(*event_devices, devices);
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
    fn live_axis_mapping_and_stage_commands_persist_verified_values() {
        let mut state = sample_device_state();
        let mut prefs = store::DevicePreferences::default();
        state.settings.configuration_source = Some(protocol::ConfigurationSource::Host);
        state.settings.dpi.as_mut().unwrap().current_x = 800;
        state.settings.dpi.as_mut().unwrap().current_y = Some(1600);
        update_preferences(
            &mut prefs,
            &protocol::RequestCommand::SetDpiAxes {
                device_id: "mouse".into(),
                x: 800,
                y: 1600,
            },
            &state,
        );
        assert_eq!(prefs.host.dpi, Some(800));
        assert_eq!(prefs.host.dpi_y, Some(1600));
        state.settings.mouse_button_mapping = Some(vec![1, 2, 3, 5, 4]);
        update_preferences(
            &mut prefs,
            &protocol::RequestCommand::SetMouseButtonMapping {
                device_id: "mouse".into(),
                mapping: vec![1, 2, 3, 5, 4],
            },
            &state,
        );
        assert_eq!(prefs.host.button_mapping, Some(vec![1, 2, 3, 5, 4]));
        let saved_host = prefs.host.clone();
        state.settings.configuration_source = Some(protocol::ConfigurationSource::Onboard {
            active_profile: Some(1),
        });
        state.settings.onboard_dpi_stage = Some(2);
        update_preferences(
            &mut prefs,
            &protocol::RequestCommand::SetOnboardDpiStage {
                device_id: "mouse".into(),
                index: 2,
            },
            &state,
        );
        assert_eq!(prefs.onboard_dpi_stage, Some(2));
        assert_eq!(prefs.host, saved_host);
    }

    #[test]
    fn selecting_onboard_profile_preserves_saved_host_preferences() {
        let mut state = sample_device_state();
        let mut preferences = store::DevicePreferences {
            onboard_dpi_stage: None,
            control: Some(store::ControlPreference::Host),
            host: capture_protocol_host_preferences(&state),
            lighting: None,
            nickname: None,
            color: Default::default(),
            sort_order: None,
            cached_model_name: None,
            usb_identity: None,
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

    #[test]
    fn unique_offline_usb_identity_restores_all_stored_metadata() {
        let (_directory, mut settings) = metadata_store();
        let identity = test_usb_identity(Some("receiver-1"));
        remember_test_device(&mut settings, "hardware-1", identity.clone());

        let metadata = resolve_device_metadata(&settings, &identity, None);

        assert_eq!(metadata.hardware_id.as_deref(), Some("hardware-1"));
        let preferences = metadata.preferences.unwrap();
        assert_eq!(preferences.nickname.as_deref(), Some("Desk mouse"));
        assert_eq!(preferences.sort_order, Some(2));
        assert_eq!(
            preferences.cached_model_name.as_deref(),
            Some("PRO X Superlight 2")
        );
    }

    #[test]
    fn unknown_or_ambiguous_offline_usb_identity_has_no_metadata() {
        let (_directory, mut settings) = metadata_store();
        let identity = test_usb_identity(Some("receiver-1"));
        let unknown = resolve_device_metadata(&settings, &identity, None);
        assert_eq!(unknown.hardware_id, None);
        assert_eq!(unknown.preferences, None);

        remember_test_device(&mut settings, "hardware-1", identity.clone());
        remember_test_device(&mut settings, "hardware-2", identity.clone());
        let ambiguous = resolve_device_metadata(&settings, &identity, None);
        assert_eq!(ambiguous.hardware_id, None);
        assert_eq!(ambiguous.preferences, None);
    }

    #[test]
    fn live_hardware_identity_wins_over_stored_usb_metadata() {
        let (_directory, mut settings) = metadata_store();
        let identity = test_usb_identity(Some("receiver-1"));
        remember_test_device(&mut settings, "stored-hardware", identity.clone());

        let metadata = resolve_device_metadata(&settings, &identity, Some("live-hardware"));

        assert_eq!(metadata.hardware_id.as_deref(), Some("live-hardware"));
        assert_eq!(metadata.preferences, None);
    }

    #[test]
    fn offline_learned_nickname_uses_stored_hardware_identity() {
        let (_directory, mut settings) = metadata_store();
        let identity = test_usb_identity(Some("receiver-1"));
        remember_test_device(&mut settings, "hardware-1", identity.clone());
        let hardware_id = resolve_device_metadata(&settings, &identity, None)
            .hardware_id
            .unwrap();

        update_nickname(
            &mut settings,
            &hardware_id,
            Some("Offline desk mouse".to_owned()),
        )
        .unwrap();

        assert_eq!(
            settings.device("hardware-1").unwrap().nickname.as_deref(),
            Some("Offline desk mouse")
        );
        assert_eq!(settings.device("transient-session-id"), None);
    }

    #[test]
    fn unknown_or_ambiguous_offline_nickname_has_no_hardware_identity() {
        let (_directory, mut settings) = metadata_store();
        let identity = test_usb_identity(Some("receiver-1"));

        assert_eq!(
            resolve_device_metadata(&settings, &identity, None).hardware_id,
            None
        );

        remember_test_device(&mut settings, "hardware-1", identity.clone());
        remember_test_device(&mut settings, "hardware-2", identity.clone());
        assert_eq!(
            resolve_device_metadata(&settings, &identity, None).hardware_id,
            None
        );
        assert_eq!(settings.device("transient-session-id"), None);
    }
}
