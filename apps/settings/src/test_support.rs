//! Test fixtures only; never included in the application.
use gflick_protocol::*;
pub fn device() -> DeviceState {
    DeviceState {
        device: DeviceSummary {
            id: "mouse".into(),
            hardware_id: Some("hardware".into()),
            vendor_id: 0x046d,
            product_id: 0xc09b,
            product_name: Some("USB Receiver".into()),
            display_name: Some("PRO X 2".into()),
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
            supported_dpi: Some(vec![400, 800, 1200, 1600, 3200]),
            polling_rates: PollingRateCapabilities::PerConnection {
                wired_hz: vec![125, 500, 1000],
                wireless_hz: vec![125, 500, 1000, 2000],
            },
            onboard_profiles: true,
            onboard_profile_description: Some(OnboardProfileDescription {
                memory_model_id: 1,
                profile_format_id: 1,
                macro_format_id: 1,
                profile_count: 5,
                factory_profile_count: 1,
                button_count: 5,
                sector_count: 6,
                sector_size: 256,
            }),
            lift_off_distance: true,
            surface_mode: true,
            operating_mode_switch: true,
            color_led_effects: true,
            bunny_hopping: true,
            mouse_button_filter: false,
        },
        settings: SettingsState {
            battery: Some(BatteryState {
                percentage: 47,
                level_code: 0,
                status_code: 0,
                status: "discharging".into(),
            }),
            dpi: Some(DpiState {
                current_x: 800,
                default_x: 800,
                current_y: Some(800),
                default_y: Some(800),
                lift_off_distance: Some(LiftOffDistance::High),
            }),
            polling_rate: PollingRateState::PerConnection {
                wired_hz: 1000,
                wireless_hz: 1000,
            },
            configuration_source: Some(ConfigurationSource::Onboard {
                active_profile: Some(1),
            }),
            operating_mode: Some(OperatingMode::Performance),
            surface_mode: Some(SurfaceMode::Off),
            bunny_hopping: Some(BunnyHoppingState {
                enabled: false,
                timeout_ms: 100,
            }),
        },
    }
}
