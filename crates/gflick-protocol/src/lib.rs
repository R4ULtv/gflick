//! Versioned JSON protocol shared by the GFlick agent and settings clients.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 2;
pub const LOCAL_SOCKET_NAME: &str = "gflick-agent-v2.sock";
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
    Shutdown,
    ListDevices,
    ListSavedDevices,
    GetAppPreferences,
    SetAppPreferences {
        preferences: AppPreferences,
    },
    GetDevice {
        device_id: String,
    },
    Subscribe,
    /// Event subscription requesting foreground battery polling for its lifetime.
    SubscribeSettings,
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
    /// Stores the enclosure color on the host. This never touches the device.
    SetDeviceColor {
        device_id: String,
        color: DeviceColor,
    },
    /// Sets or clears a host-side display name. This never touches the device.
    SetDeviceNickname {
        device_id: String,
        /// `None` clears the nickname and restores the reported name.
        nickname: Option<String>,
    },
    /// Reorders the device list. The order is host-side and keyed by hardware
    /// identity, so it survives reconnects and USB-path changes.
    ReorderDevices {
        /// Ordered saved IDs; omissions clear positions, and duplicates/unknowns fail.
        hardware_ids: Vec<String>,
    },
}

/// Host application preferences stored by the agent alongside mouse settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppPreferences {
    pub confirm_apply: bool,
    pub confirm_discard: bool,
    pub confirm_profile_writes: bool,
    pub startup_defaults_applied: bool,
}
impl AppPreferences {
    pub fn needs_confirmation(&self, discard: bool, profile_write: bool) -> bool {
        if discard {
            self.confirm_discard
        } else {
            self.confirm_apply || (self.confirm_profile_writes && profile_write)
        }
    }
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
    AppPreferences { preferences: AppPreferences },
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
    ApplicationShuttingDown,
    DeviceConnected {
        device: DeviceSummary,
    },
    DeviceReady {
        device: Box<DeviceState>,
    },
    DeviceUnavailable {
        device_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code: Option<DeviceUnavailableReason>,
        reason: String,
    },
    SettingsChanged {
        device: Box<DeviceState>,
    },
    /// Authoritative, host-side presentation metadata in display order.
    DeviceMetadataChanged {
        devices: Vec<DeviceSummary>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceUnavailableReason {
    NotResponding,
    CommunicationError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceAvailability {
    Initializing,
    Ready,
    Unavailable {
        reason: DeviceUnavailableReason,
        detail: String,
    },
}

/// Cosmetic enclosure color, stored on the host rather than written over HID++.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceColor {
    #[default]
    Black,
    White,
    Magenta,
    Cyan,
    Red,
    Blue,
    Lilac,
    Mint,
}
impl DeviceColor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::White => "white",
            Self::Magenta => "magenta",
            Self::Cyan => "cyan",
            Self::Red => "red",
            Self::Blue => "blue",
            Self::Lilac => "lilac",
            Self::Mint => "mint",
        }
    }
}

/// Models for which GFlick has verified enclosure artwork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceModel {
    Superlight,
    Superlight2,
    G305,
}
impl DeviceModel {
    pub fn colors(self) -> &'static [DeviceColor] {
        use DeviceColor::*;
        match self {
            Self::Superlight => &[White, Black, Red, Magenta],
            Self::Superlight2 => &[White, Black, Cyan, Magenta],
            Self::G305 => &[White, Black, Lilac, Blue, Mint],
        }
    }
    /// User-specified sRGB swatch values. See assets/mouses/SOURCES.md.
    pub fn swatch(self, color: DeviceColor) -> Option<u32> {
        use DeviceColor::*;
        if !self.colors().contains(&color) {
            return None;
        }
        Some(match color {
            White => 0xffffff,
            Black => 0x000000,
            Magenta => 0xd62975,
            Cyan => 0x017bb9,
            Red => 0xe63439,
            Blue => 0x0072ce,
            Lilac => 0xafbded,
            Mint => 0x28b8b0,
        })
    }
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
    /// HID++ device-reported model name. Unlike `product_name`, this identifies the
    /// paired mouse when the USB interface itself is a generic receiver.
    #[serde(default)]
    pub display_name: Option<String>,
    pub serial_number: Option<String>,
    /// User-assigned display name. Host-side only: it is stored with the agent's
    /// preferences and never written to the device.
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub color: DeviceColor,
    /// User-assigned list position. `None` sorts after every ordered device.
    #[serde(default)]
    pub sort_order: Option<u32>,
    pub connection: DeviceConnection,
    pub device_index: u8,
    /// Structured lifecycle state for new clients. `ready` remains available for
    /// protocol-v1 clients that predate this additive field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<DeviceAvailability>,
    pub ready: bool,
}

impl DeviceSummary {
    pub fn model(&self) -> Option<DeviceModel> {
        if self.vendor_id != 0x046d {
            return None;
        }
        let name = self
            .display_name
            .as_ref()
            .or(self.product_name.as_ref())?
            .trim()
            .to_ascii_lowercase();
        match name.as_str() {
            "pro x 2" | "pro x superlight 2" => Some(DeviceModel::Superlight2),
            "pro x wireless" | "pro x superlight" => Some(DeviceModel::Superlight),
            "g305"
            | "g305 lightspeed"
            | "g305 lightspeed wireless gaming mouse"
            | "g304"
            | "g304 lightspeed"
            | "g304 lightspeed wireless gaming mouse" => Some(DeviceModel::G305),
            _ => None,
        }
    }
    pub fn available_colors(&self) -> &'static [DeviceColor] {
        self.model().map(DeviceModel::colors).unwrap_or(&[])
    }
    pub fn supports_color_selection(&self) -> bool {
        !self.available_colors().is_empty()
    }
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
    fn catalog_keeps_model_colors_and_swatches_separate() {
        use DeviceColor::*;
        assert_eq!(
            DeviceModel::G305.colors(),
            &[White, Black, Lilac, Blue, Mint]
        );
        assert_eq!(
            DeviceModel::Superlight.colors(),
            &[White, Black, Red, Magenta]
        );
        assert_eq!(DeviceModel::Superlight2.swatch(Magenta), Some(0xd62975));
        assert_eq!(DeviceModel::Superlight.swatch(Magenta), Some(0xd62975));
        assert_eq!(DeviceModel::G305.swatch(Mint), Some(0x28b8b0));
        assert_eq!(DeviceModel::G305.swatch(Cyan), None);
        assert_eq!(DeviceModel::Superlight2.swatch(Red), None);
    }

    #[test]
    fn color_wire_format_rejects_unknown_variants() {
        for color in [
            DeviceColor::Black,
            DeviceColor::White,
            DeviceColor::Magenta,
            DeviceColor::Cyan,
            DeviceColor::Red,
            DeviceColor::Blue,
            DeviceColor::Lilac,
            DeviceColor::Mint,
        ] {
            let command = RequestCommand::SetDeviceColor {
                device_id: "mouse".into(),
                color,
            };
            let json = serde_json::to_string(&command).unwrap();
            assert_eq!(
                serde_json::from_str::<RequestCommand>(&json).unwrap(),
                command
            );
            assert!(json.contains(color.as_str()));
        }
        assert!(serde_json::from_str::<DeviceColor>("\"orange\"").is_err());
    }

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
    fn host_metadata_requests_round_trip() {
        let rename = ClientRequest {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
            command: RequestCommand::SetDeviceNickname {
                device_id: "mouse-1".to_owned(),
                nickname: Some("Desk left".to_owned()),
            },
        };
        let json = serde_json::to_string(&rename).unwrap();
        assert!(json.contains("\"command\":\"set_device_nickname\""));
        assert_eq!(
            serde_json::from_str::<ClientRequest>(&json).unwrap(),
            rename
        );

        let reorder = ClientRequest {
            id: 2,
            protocol_version: PROTOCOL_VERSION,
            command: RequestCommand::ReorderDevices {
                hardware_ids: vec!["a".to_owned(), "b".to_owned()],
            },
        };
        let json = serde_json::to_string(&reorder).unwrap();
        assert!(json.contains("\"command\":\"reorder_devices\""));
        assert_eq!(
            serde_json::from_str::<ClientRequest>(&json).unwrap(),
            reorder
        );
    }

    /// The host-metadata fields are additive, so a payload from an agent that
    /// predates them must still deserialize.
    #[test]
    fn device_summary_defaults_host_metadata_when_absent() {
        let json = r#"{
            "id": "mouse-1",
            "vendor_id": 1133,
            "product_id": 50253,
            "product_name": "USB Receiver",
            "serial_number": null,
            "connection": "receiver",
            "device_index": 1,
            "ready": true
        }"#;
        let summary = serde_json::from_str::<DeviceSummary>(json).unwrap();
        assert_eq!(summary.nickname, None);
        assert_eq!(summary.color, DeviceColor::Black);
        assert_eq!(summary.sort_order, None);
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
    fn metadata_event_round_trip_preserves_the_exact_tag_and_order() {
        let first = DeviceSummary {
            id: "first".to_owned(),
            hardware_id: Some("hardware-first".to_owned()),
            vendor_id: 0x046d,
            product_id: 0xc54d,
            product_name: Some("Receiver".to_owned()),
            display_name: Some("First mouse".to_owned()),
            serial_number: None,
            nickname: Some("Desk".to_owned()),
            color: Default::default(),
            sort_order: Some(0),
            connection: DeviceConnection::Receiver,
            device_index: 1,
            availability: Some(DeviceAvailability::Ready),
            ready: true,
        };
        let mut second = first.clone();
        second.id = "second".to_owned();
        second.hardware_id = Some("hardware-second".to_owned());
        second.sort_order = Some(1);

        let message = ServerMessage::event(AgentEvent::DeviceMetadataChanged {
            devices: vec![first, second],
        });
        let json = serde_json::to_string(&message).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["event"], "device_metadata_changed");
        assert_eq!(value["devices"][0]["id"], "first");
        assert_eq!(value["devices"][1]["id"], "second");
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            message
        );
    }

    #[test]
    fn application_shutdown_round_trip_is_stable() {
        let request = ClientRequest {
            id: 7,
            protocol_version: PROTOCOL_VERSION,
            command: RequestCommand::Shutdown,
        };
        let request_json = serde_json::to_string(&request).unwrap();
        assert!(request_json.contains("\"command\":\"shutdown\""));
        assert_eq!(
            serde_json::from_str::<ClientRequest>(&request_json).unwrap(),
            request
        );

        let event = ServerMessage::event(AgentEvent::ApplicationShuttingDown);
        let event_json = serde_json::to_string(&event).unwrap();
        assert!(event_json.contains("\"event\":\"application_shutting_down\""));
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&event_json).unwrap(),
            event
        );
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
        assert_eq!(summary.display_name, None);
        assert_eq!(summary.availability, None);

        summary.hardware_id = Some("046d:unit:1234abcd".to_owned());
        summary.availability = Some(DeviceAvailability::Ready);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"hardware_id\":\"046d:unit:1234abcd\""));
        assert!(json.contains("\"availability\":{\"state\":\"ready\"}"));
    }

    #[test]
    fn unavailable_event_keeps_reason_code_backward_compatible() {
        let old_json = r#"{
            "message":"event",
            "protocol_version":1,
            "event":"device_unavailable",
            "device_id":"mouse-1",
            "reason":"timed out"
        }"#;
        let event: ServerMessage = serde_json::from_str(old_json).unwrap();
        assert!(matches!(
            event,
            ServerMessage::Event {
                event: AgentEvent::DeviceUnavailable {
                    reason_code: None,
                    ..
                },
                ..
            }
        ));
    }
}
