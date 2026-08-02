//! Versioned JSON protocol shared by the Open Hub agent and settings clients.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const LOCAL_SOCKET_NAME: &str = "open-hub-agent-v1.sock";
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRequest {
    pub id: u64,
    pub protocol_version: u16,
    #[serde(flatten)]
    pub command: RequestCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum RequestCommand {
    Ping,
    ListDevices,
    GetDevice {
        device_id: String,
    },
    Subscribe,
    SetDpi {
        device_id: String,
        dpi: u16,
    },
    SetPollingRate {
        device_id: String,
        connection: Connection,
        hz: u16,
        #[serde(default)]
        disable_onboard_profiles: bool,
    },
    SetLiftOffDistance {
        device_id: String,
        lod: LiftOffDistance,
    },
    SetSurfaceMode {
        device_id: String,
        mode: SurfaceMode,
    },
    SetOperatingMode {
        device_id: String,
        mode: OperatingMode,
    },
    SetBunnyHopping {
        device_id: String,
        enabled: bool,
        timeout_ms: u16,
    },
    SetLighting {
        device_id: String,
        #[serde(default)]
        zone: u8,
        effect: LightingEffect,
    },
    UseFirmwareLighting {
        device_id: String,
    },
    UseHostSettings {
        device_id: String,
    },
    UseOnboardProfile {
        device_id: String,
        profile: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    Wired,
    Wireless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiftOffDistance {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceMode {
    On,
    Automatic,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingMode {
    Endurance,
    Performance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum LightingEffect {
    Disabled,
    Fixed {
        color: RgbColor,
    },
    Cycling {
        period_ms: u16,
        brightness: u8,
    },
    Breathing {
        color: RgbColor,
        period_ms: u16,
        brightness: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum ServerMessage {
    Response {
        id: u64,
        protocol_version: u16,
        result: ResponseResult,
    },
    Event {
        protocol_version: u16,
        #[serde(flatten)]
        event: AgentEvent,
    },
}

impl ServerMessage {
    pub fn success(id: u64, data: ResponseData) -> Self {
        Self::Response {
            id,
            protocol_version: PROTOCOL_VERSION,
            result: ResponseResult::Success { data },
        }
    }

    pub fn error(id: u64, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Response {
            id,
            protocol_version: PROTOCOL_VERSION,
            result: ResponseResult::Error {
                code,
                message: message.into(),
            },
        }
    }

    pub fn event(event: AgentEvent) -> Self {
        Self::Event {
            protocol_version: PROTOCOL_VERSION,
            event,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseResult {
    Success { data: ResponseData },
    Error { code: ErrorCode, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseData {
    Pong,
    Subscribed,
    Acknowledged,
    Devices { devices: Vec<DeviceSummary> },
    Device { device: Box<DeviceState> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    VersionMismatch,
    DeviceNotFound,
    DeviceUnavailable,
    OperationFailed,
    PersistenceFailed,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    DeviceConnected {
        device: DeviceSummary,
    },
    DeviceReady {
        device: Box<DeviceState>,
    },
    DeviceUnavailable {
        device_id: String,
        reason: String,
    },
    SettingsChanged {
        device: Box<DeviceState>,
    },
    DeviceDisconnected {
        device_id: String,
    },
    BatteryChanged {
        device_id: String,
        battery: Option<BatteryState>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceConnection {
    DirectUsb,
    Receiver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    /// Session-scoped routing ID. It may include a USB-path hash and must not be used
    /// as the key for persisted preferences.
    pub id: String,
    /// Opaque physical-device identity derived from HID++ data. It is available after
    /// the agent opens the mouse and remains stable across transports and USB paths.
    #[serde(default)]
    pub hardware_id: Option<String>,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_name: Option<String>,
    pub serial_number: Option<String>,
    pub connection: DeviceConnection,
    pub device_index: u8,
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceState {
    pub device: DeviceSummary,
    pub capabilities: DeviceCapabilities,
    pub settings: SettingsState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub battery: bool,
    pub supported_dpi: Option<Vec<u16>>,
    pub polling_rates: PollingRateCapabilities,
    pub onboard_profiles: bool,
    pub onboard_profile_description: Option<OnboardProfileDescription>,
    pub lift_off_distance: bool,
    pub surface_mode: bool,
    pub operating_mode_switch: bool,
    pub color_led_effects: bool,
    pub bunny_hopping: bool,
    pub mouse_button_filter: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PollingRateCapabilities {
    Unsupported,
    Shared {
        supported_hz: Vec<u16>,
    },
    PerConnection {
        wired_hz: Vec<u16>,
        wireless_hz: Vec<u16>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileDescription {
    pub memory_model_id: u8,
    pub profile_format_id: u8,
    pub macro_format_id: u8,
    pub profile_count: u8,
    pub factory_profile_count: u8,
    pub button_count: u8,
    pub sector_count: u8,
    pub sector_size: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsState {
    pub battery: Option<BatteryState>,
    pub dpi: Option<DpiState>,
    pub polling_rate: PollingRateState,
    pub configuration_source: Option<ConfigurationSource>,
    pub operating_mode: Option<OperatingMode>,
    pub surface_mode: Option<SurfaceMode>,
    pub bunny_hopping: Option<BunnyHoppingState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryState {
    pub percentage: u8,
    pub level_code: u8,
    pub status_code: u8,
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiState {
    pub current_x: u16,
    pub default_x: u16,
    pub current_y: Option<u16>,
    pub default_y: Option<u16>,
    pub lift_off_distance: Option<LiftOffDistance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PollingRateState {
    Unsupported,
    Shared { hz: u16 },
    PerConnection { wired_hz: u16, wireless_hz: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ConfigurationSource {
    Host,
    Onboard { active_profile: Option<u16> },
    Unknown { raw_mode: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BunnyHoppingState {
    pub enabled: bool,
    pub timeout_ms: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_is_stable() {
        let request = ClientRequest {
            id: 42,
            protocol_version: PROTOCOL_VERSION,
            command: RequestCommand::SetDpi {
                device_id: "mouse-1".to_owned(),
                dpi: 1600,
            },
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientRequest>(&json).unwrap(),
            request
        );
        assert!(json.contains("\"command\":\"set_dpi\""));
    }

    #[test]
    fn event_round_trip_is_stable() {
        let message = ServerMessage::event(AgentEvent::BatteryChanged {
            device_id: "mouse-1".to_owned(),
            battery: Some(BatteryState {
                percentage: 89,
                level_code: 8,
                status_code: 0,
                status: "discharging".to_owned(),
            }),
        });
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            message
        );
        assert!(json.contains("\"message\":\"event\""));
    }

    #[test]
    fn device_summary_keeps_hardware_identity_backward_compatible() {
        let old_json = r#"{
            "id":"046d:c53f:path-example:01",
            "vendor_id":1133,
            "product_id":50495,
            "product_name":"USB Receiver",
            "serial_number":null,
            "connection":"receiver",
            "device_index":1,
            "ready":true
        }"#;
        let mut summary: DeviceSummary = serde_json::from_str(old_json).unwrap();
        assert_eq!(summary.hardware_id, None);

        summary.hardware_id = Some("046d:unit:1234abcd".to_owned());
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"hardware_id\":\"046d:unit:1234abcd\""));
    }
}
