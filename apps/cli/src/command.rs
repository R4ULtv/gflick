use std::io::Write;

use anyhow::{Result, bail};
use gflick_protocol::{
    Connection, DeviceState, DeviceSummary, LiftOffDistance, LightingEffect, OperatingMode,
    RequestCommand, ResponseData, SurfaceMode,
};

use crate::{
    args::{
        Command, ConnectionChoice, DeviceCommand, DeviceSetArgs, DeviceUseArgs,
        LiftOffDistanceChoice, LightingCommand, OperatingModeChoice, OutputFormat, SettingCommand,
        SurfaceModeChoice, UseCommand,
    },
    output::{write_device, write_devices, write_status},
    selector::select_device,
};

pub trait Backend {
    fn request(&mut self, command: RequestCommand) -> Result<ResponseData>;
}

pub struct IpcBackend;

impl Backend for IpcBackend {
    fn request(&mut self, command: RequestCommand) -> Result<ResponseData> {
        gflick_client::request(command)
    }
}

pub fn execute(
    backend: &mut impl Backend,
    command: Command,
    format: OutputFormat,
    writer: impl Write,
) -> Result<()> {
    let mut writer = writer;
    match command {
        Command::Status => {
            expect_pong(backend.request(RequestCommand::Ping)?)?;
            write_status(&mut writer, format)
        }
        Command::Device { command } => execute_device(backend, command, format, &mut writer),
        Command::Events { .. } | Command::Completions { .. } => unreachable!("handled by main"),
    }
}

fn execute_device(
    backend: &mut impl Backend,
    command: DeviceCommand,
    format: OutputFormat,
    writer: &mut impl Write,
) -> Result<()> {
    match command {
        DeviceCommand::List => write_devices(writer, format, &list_devices(backend)?),
        DeviceCommand::Show { selector } => {
            let device = resolve(backend, selector.as_deref())?;
            write_device(writer, format, &get_device(backend, &device.id)?)
        }
        DeviceCommand::Set(args) => {
            let device = resolve(backend, args.selector.as_deref())?;
            let response = backend.request(setting_request(args, device.id.clone()))?;
            write_device(writer, format, &expect_device(response)?)
        }
        DeviceCommand::Use(args) => {
            let device = resolve(backend, args.selector.as_deref())?;
            let response = backend.request(use_request(args, device.id.clone()))?;
            write_device(writer, format, &expect_device(response)?)
        }
    }
}

fn list_devices(backend: &mut impl Backend) -> Result<Vec<DeviceSummary>> {
    match backend.request(RequestCommand::ListDevices)? {
        ResponseData::Devices { devices } => Ok(devices),
        response => bail!("agent returned an unexpected device-list response: {response:?}"),
    }
}

fn resolve(backend: &mut impl Backend, selector: Option<&str>) -> Result<DeviceSummary> {
    select_device(&list_devices(backend)?, selector)
}

fn get_device(backend: &mut impl Backend, device_id: &str) -> Result<DeviceState> {
    expect_device(backend.request(RequestCommand::GetDevice {
        device_id: device_id.to_owned(),
    })?)
}

fn expect_pong(response: ResponseData) -> Result<()> {
    if matches!(response, ResponseData::Pong) {
        Ok(())
    } else {
        bail!("agent returned an unexpected ping response: {response:?}")
    }
}

fn expect_device(response: ResponseData) -> Result<DeviceState> {
    match response {
        ResponseData::Device { device } => Ok(*device),
        response => bail!("agent returned an unexpected device response: {response:?}"),
    }
}

fn setting_request(args: DeviceSetArgs, device_id: String) -> RequestCommand {
    match args.setting {
        SettingCommand::Dpi { dpi } => RequestCommand::SetDpi { device_id, dpi },
        SettingCommand::PollingRate {
            hz,
            connection,
            disable_onboard_profiles,
        } => RequestCommand::SetPollingRate {
            device_id,
            connection: connection_to_protocol(connection),
            hz,
            disable_onboard_profiles,
        },
        SettingCommand::LiftOffDistance { lod } => RequestCommand::SetLiftOffDistance {
            device_id,
            lod: lod_to_protocol(lod),
        },
        SettingCommand::SurfaceMode { mode } => RequestCommand::SetSurfaceMode {
            device_id,
            mode: surface_to_protocol(mode),
        },
        SettingCommand::OperatingMode { mode } => RequestCommand::SetOperatingMode {
            device_id,
            mode: operating_to_protocol(mode),
        },
        SettingCommand::Bhop { state, window_ms } => RequestCommand::SetBunnyHopping {
            device_id,
            enabled: state.enabled(),
            timeout_ms: window_ms,
        },
        SettingCommand::Lighting { effect } => RequestCommand::SetLighting {
            device_id,
            zone: 0,
            effect: lighting_to_protocol(effect),
        },
    }
}

fn use_request(args: DeviceUseArgs, device_id: String) -> RequestCommand {
    match args.mode {
        UseCommand::FirmwareLighting => RequestCommand::UseFirmwareLighting { device_id },
        UseCommand::HostSettings => RequestCommand::UseHostSettings { device_id },
        UseCommand::OnboardProfile { sector } => RequestCommand::UseOnboardProfile {
            device_id,
            profile: sector,
        },
    }
}

const fn connection_to_protocol(value: ConnectionChoice) -> Connection {
    match value {
        ConnectionChoice::Wired => Connection::Wired,
        ConnectionChoice::Wireless => Connection::Wireless,
    }
}

const fn lod_to_protocol(value: LiftOffDistanceChoice) -> LiftOffDistance {
    match value {
        LiftOffDistanceChoice::Low => LiftOffDistance::Low,
        LiftOffDistanceChoice::Medium => LiftOffDistance::Medium,
        LiftOffDistanceChoice::High => LiftOffDistance::High,
    }
}

const fn surface_to_protocol(value: SurfaceModeChoice) -> SurfaceMode {
    match value {
        SurfaceModeChoice::On => SurfaceMode::On,
        SurfaceModeChoice::Automatic => SurfaceMode::Automatic,
        SurfaceModeChoice::Off => SurfaceMode::Off,
    }
}

const fn operating_to_protocol(value: OperatingModeChoice) -> OperatingMode {
    match value {
        OperatingModeChoice::Performance => OperatingMode::Performance,
        OperatingModeChoice::Endurance => OperatingMode::Endurance,
    }
}

fn lighting_to_protocol(value: LightingCommand) -> LightingEffect {
    match value {
        LightingCommand::Off => LightingEffect::Disabled,
        LightingCommand::Fixed { color } => LightingEffect::Fixed { color },
        LightingCommand::Cycling {
            period_ms,
            brightness,
        } => LightingEffect::Cycling {
            period_ms,
            brightness,
        },
        LightingCommand::Breathing {
            color,
            period_ms,
            brightness,
        } => LightingEffect::Breathing {
            color,
            period_ms,
            brightness,
        },
    }
}

pub fn ipc_context(error: anyhow::Error) -> anyhow::Error {
    let text = error.to_string();
    if text.contains("could not connect to the GFlick agent") {
        anyhow::anyhow!(
            "the GFlick background agent is not running; install or start it, then try again"
        )
    } else {
        error.context("GFlick command failed")
    }
}

#[cfg(test)]
mod tests {
    use gflick_protocol::{
        DeviceCapabilities, DeviceConnection, PollingRateCapabilities, PollingRateState,
        SettingsState,
    };

    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        requests: Vec<RequestCommand>,
    }

    impl Backend for FakeBackend {
        fn request(&mut self, command: RequestCommand) -> Result<ResponseData> {
            let response = match &command {
                RequestCommand::Ping => ResponseData::Pong,
                RequestCommand::ListDevices => ResponseData::Devices {
                    devices: vec![summary()],
                },
                _ => ResponseData::Device {
                    device: Box::new(state()),
                },
            };
            self.requests.push(command);
            Ok(response)
        }
    }

    fn summary() -> DeviceSummary {
        DeviceSummary {
            id: "session-id".to_owned(),
            hardware_id: Some("hardware-id".to_owned()),
            vendor_id: 1,
            product_id: 2,
            product_name: Some("Mouse".to_owned()),
            display_name: None,
            serial_number: None,
            connection: DeviceConnection::DirectUsb,
            device_index: 0,
            availability: None,
            ready: true,
        }
    }

    fn state() -> DeviceState {
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

    fn set(setting: SettingCommand) -> Command {
        Command::Device {
            command: DeviceCommand::Set(DeviceSetArgs {
                selector: Some("hardware-id".to_owned()),
                setting,
            }),
        }
    }

    #[test]
    fn maps_every_setting_request_exactly() {
        let commands = vec![
            set(SettingCommand::Dpi { dpi: 1600 }),
            set(SettingCommand::PollingRate {
                hz: 1000,
                connection: ConnectionChoice::Wired,
                disable_onboard_profiles: true,
            }),
            set(SettingCommand::LiftOffDistance {
                lod: LiftOffDistanceChoice::Low,
            }),
            set(SettingCommand::SurfaceMode {
                mode: SurfaceModeChoice::Automatic,
            }),
            set(SettingCommand::OperatingMode {
                mode: OperatingModeChoice::Performance,
            }),
            set(SettingCommand::Bhop {
                state: crate::args::Toggle::On,
                window_ms: 100,
            }),
            set(SettingCommand::Lighting {
                effect: LightingCommand::Off,
            }),
            set(SettingCommand::Lighting {
                effect: LightingCommand::Fixed {
                    color: gflick_protocol::RgbColor {
                        red: 1,
                        green: 2,
                        blue: 3,
                    },
                },
            }),
            set(SettingCommand::Lighting {
                effect: LightingCommand::Cycling {
                    period_ms: 1000,
                    brightness: 100,
                },
            }),
            set(SettingCommand::Lighting {
                effect: LightingCommand::Breathing {
                    color: gflick_protocol::RgbColor {
                        red: 1,
                        green: 2,
                        blue: 3,
                    },
                    period_ms: 1000,
                    brightness: 100,
                },
            }),
        ];
        let mut fake = FakeBackend::default();
        for command in commands {
            execute(&mut fake, command, OutputFormat::Json, Vec::new()).unwrap();
        }
        let mutations = fake
            .requests
            .into_iter()
            .filter(|request| {
                matches!(
                    request,
                    RequestCommand::SetDpi { .. }
                        | RequestCommand::SetPollingRate { .. }
                        | RequestCommand::SetLiftOffDistance { .. }
                        | RequestCommand::SetSurfaceMode { .. }
                        | RequestCommand::SetOperatingMode { .. }
                        | RequestCommand::SetBunnyHopping { .. }
                        | RequestCommand::SetLighting { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            mutations,
            vec![
                RequestCommand::SetDpi {
                    device_id: "session-id".to_owned(),
                    dpi: 1600
                },
                RequestCommand::SetPollingRate {
                    device_id: "session-id".to_owned(),
                    connection: Connection::Wired,
                    hz: 1000,
                    disable_onboard_profiles: true
                },
                RequestCommand::SetLiftOffDistance {
                    device_id: "session-id".to_owned(),
                    lod: LiftOffDistance::Low
                },
                RequestCommand::SetSurfaceMode {
                    device_id: "session-id".to_owned(),
                    mode: SurfaceMode::Automatic
                },
                RequestCommand::SetOperatingMode {
                    device_id: "session-id".to_owned(),
                    mode: OperatingMode::Performance
                },
                RequestCommand::SetBunnyHopping {
                    device_id: "session-id".to_owned(),
                    enabled: true,
                    timeout_ms: 100
                },
                RequestCommand::SetLighting {
                    device_id: "session-id".to_owned(),
                    zone: 0,
                    effect: LightingEffect::Disabled
                },
                RequestCommand::SetLighting {
                    device_id: "session-id".to_owned(),
                    zone: 0,
                    effect: LightingEffect::Fixed {
                        color: gflick_protocol::RgbColor {
                            red: 1,
                            green: 2,
                            blue: 3
                        }
                    }
                },
                RequestCommand::SetLighting {
                    device_id: "session-id".to_owned(),
                    zone: 0,
                    effect: LightingEffect::Cycling {
                        period_ms: 1000,
                        brightness: 100
                    }
                },
                RequestCommand::SetLighting {
                    device_id: "session-id".to_owned(),
                    zone: 0,
                    effect: LightingEffect::Breathing {
                        color: gflick_protocol::RgbColor {
                            red: 1,
                            green: 2,
                            blue: 3
                        },
                        period_ms: 1000,
                        brightness: 100
                    }
                },
            ]
        );
    }

    #[test]
    fn maps_every_use_request() {
        let commands = [
            UseCommand::FirmwareLighting,
            UseCommand::HostSettings,
            UseCommand::OnboardProfile { sector: 3 },
        ];
        let mut fake = FakeBackend::default();
        for mode in commands {
            execute(
                &mut fake,
                Command::Device {
                    command: DeviceCommand::Use(DeviceUseArgs {
                        selector: None,
                        mode,
                    }),
                },
                OutputFormat::Human,
                Vec::new(),
            )
            .unwrap();
        }
        let uses = fake
            .requests
            .into_iter()
            .filter(|request| {
                matches!(
                    request,
                    RequestCommand::UseFirmwareLighting { .. }
                        | RequestCommand::UseHostSettings { .. }
                        | RequestCommand::UseOnboardProfile { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            uses,
            vec![
                RequestCommand::UseFirmwareLighting {
                    device_id: "session-id".to_owned()
                },
                RequestCommand::UseHostSettings {
                    device_id: "session-id".to_owned()
                },
                RequestCommand::UseOnboardProfile {
                    device_id: "session-id".to_owned(),
                    profile: 3
                },
            ]
        );
    }

    #[test]
    fn rejects_unexpected_response_shape() {
        struct Wrong;
        impl Backend for Wrong {
            fn request(&mut self, _: RequestCommand) -> Result<ResponseData> {
                Ok(ResponseData::Acknowledged)
            }
        }
        assert!(execute(&mut Wrong, Command::Status, OutputFormat::Human, Vec::new()).is_err());
    }
}
