//! Reusable Logitech HID++ transport and device operations for Open Hub.

mod manager;
mod mouse;

pub use manager::{DeviceChanges, DeviceConnection, DeviceManager, ManagedDevice};
pub use mouse::{
    DeviceCapabilities, MouseDevice, PollingRateCapabilities, PollingRateSettings, SettingChange,
    SettingsSnapshot,
};

use std::{
    fmt,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use hidapi::HidDevice;

const SHORT_REPORT_ID: u8 = 0x10;
const LONG_REPORT_ID: u8 = 0x11;
const SHORT_REPORT_LEN: usize = 7;
const LONG_REPORT_LEN: usize = 20;
// Keep the high bit set so replies are distinct from notifications (software ID 0).
// Avoid IDs currently used by OpenRGB (7), LGSTrayEx (A), Solaar (B), G HUB (D),
// and Logitech firmware (F).
const SOFTWARE_ID: u8 = 0x0c;

pub const FEATURE_BATTERY_STATUS: u16 = 0x1000;
pub const FEATURE_SET: u16 = 0x0001;
pub const FEATURE_DEVICE_INFORMATION: u16 = 0x0003;
pub const FEATURE_DEVICE_TYPE_AND_NAME: u16 = 0x0005;
pub const FEATURE_UNIFIED_BATTERY: u16 = 0x1004;
pub const FEATURE_ADJUSTABLE_DPI: u16 = 0x2201;
pub const FEATURE_EXTENDED_ADJUSTABLE_DPI: u16 = 0x2202;
pub const FEATURE_REPORT_RATE: u16 = 0x8060;
pub const FEATURE_EXTENDED_REPORT_RATE: u16 = 0x8061;
pub const FEATURE_COLOR_LED_EFFECTS: u16 = 0x8070;
pub const FEATURE_MODE_STATUS: u16 = 0x8090;
pub const FEATURE_BUNNY_HOPPING: u16 = 0x80e0;
pub const FEATURE_ONBOARD_PROFILES: u16 = 0x8100;
pub const FEATURE_MOUSE_BUTTON_FILTER: u16 = 0x8110;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolVersion {
    pub major: u8,
    pub minor: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureInfo {
    pub index: u8,
    pub flags: u8,
    pub version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureSetEntry {
    pub index: u8,
    pub feature_id: u16,
    pub flags: u8,
    pub version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureAudience {
    Protocol,
    UserFeature,
    Diagnostic,
    Maintenance,
    Internal,
    Unknown,
}

impl fmt::Display for FeatureAudience {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Protocol => "protocol",
            Self::UserFeature => "user feature",
            Self::Diagnostic => "diagnostic",
            Self::Maintenance => "maintenance",
            Self::Internal => "internal",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureSupport {
    Complete,
    Partial,
    Planned,
    DiagnosticOnly,
    IntentionallyBlocked,
    Unknown,
}

impl fmt::Display for FeatureSupport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Planned => "planned",
            Self::DiagnosticOnly => "diagnostic only",
            Self::IntentionallyBlocked => "blocked",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureDescriptor {
    pub name: &'static str,
    pub audience: FeatureAudience,
    pub support: FeatureSupport,
}

/// Classifies HID++ features for the public Open Hub API.
///
/// Feature-set flags take precedence over the numeric ID: Logitech devices can
/// expose an otherwise familiar feature as hidden/internal firmware machinery.
pub fn describe_feature(feature_id: u16, flags: u8) -> FeatureDescriptor {
    if flags & 0x60 != 0 {
        return FeatureDescriptor {
            name: "Firmware-only feature",
            audience: FeatureAudience::Internal,
            support: FeatureSupport::IntentionallyBlocked,
        };
    }
    let (name, audience, support) = match feature_id {
        0x0000 => ("Root", FeatureAudience::Protocol, FeatureSupport::Complete),
        0x0001 => (
            "Feature Set",
            FeatureAudience::Protocol,
            FeatureSupport::Complete,
        ),
        0x0003 => (
            "Device Information",
            FeatureAudience::Protocol,
            FeatureSupport::Complete,
        ),
        0x0005 => (
            "Device Type and Name",
            FeatureAudience::Protocol,
            FeatureSupport::Complete,
        ),
        0x0020 => (
            "Configuration Change",
            FeatureAudience::Protocol,
            FeatureSupport::Partial,
        ),
        0x00c2 => (
            "Signed DFU Control",
            FeatureAudience::Maintenance,
            FeatureSupport::IntentionallyBlocked,
        ),
        0x1004 => (
            "Unified Battery",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x1000 => (
            "Battery Status",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x1500 => (
            "Force Pairing",
            FeatureAudience::Maintenance,
            FeatureSupport::IntentionallyBlocked,
        ),
        0x1602 => (
            "Unknown Public Feature 0x1602",
            FeatureAudience::Unknown,
            FeatureSupport::Unknown,
        ),
        0x1801 => (
            "Manufacturing Mode",
            FeatureAudience::Internal,
            FeatureSupport::IntentionallyBlocked,
        ),
        0x1802 => (
            "Device Reset",
            FeatureAudience::Maintenance,
            FeatureSupport::IntentionallyBlocked,
        ),
        0x1805 => (
            "Out-of-box State",
            FeatureAudience::Internal,
            FeatureSupport::IntentionallyBlocked,
        ),
        0x1806 => (
            "Configurable Device Properties",
            FeatureAudience::Internal,
            FeatureSupport::IntentionallyBlocked,
        ),
        0x1d4b => (
            "Wireless Device Status",
            FeatureAudience::Protocol,
            FeatureSupport::Partial,
        ),
        0x1e00 => (
            "Enable Hidden Features",
            FeatureAudience::Internal,
            FeatureSupport::IntentionallyBlocked,
        ),
        0x2201 => (
            "Adjustable DPI",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x2202 => (
            "Extended Adjustable DPI",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x2250 => (
            "X/Y Motion Statistics",
            FeatureAudience::Diagnostic,
            FeatureSupport::DiagnosticOnly,
        ),
        0x2251 => (
            "Wheel Motion Statistics",
            FeatureAudience::Diagnostic,
            FeatureSupport::DiagnosticOnly,
        ),
        0x8060 => (
            "Adjustable Report Rate",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x8061 => (
            "Extended Adjustable Report Rate",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x8070 => (
            "Color LED Effects",
            FeatureAudience::UserFeature,
            FeatureSupport::Partial,
        ),
        0x8090 => (
            "Mode Status",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x80e0 => (
            "Bunny Hopping",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x8100 => (
            "Onboard Profiles",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        0x8110 => (
            "Mouse Button Filter",
            FeatureAudience::UserFeature,
            FeatureSupport::Complete,
        ),
        _ => (
            "Unknown HID++ Feature",
            FeatureAudience::Unknown,
            FeatureSupport::Unknown,
        ),
    };

    if flags & 0x60 != 0 {
        FeatureDescriptor {
            name,
            audience: FeatureAudience::Internal,
            support: FeatureSupport::IntentionallyBlocked,
        }
    } else {
        FeatureDescriptor {
            name,
            audience,
            support,
        }
    }
}

pub fn feature_flag_names(flags: u8) -> Vec<&'static str> {
    let mut names = Vec::new();
    if flags & 0x40 != 0 {
        names.push("hidden");
    }
    if flags & 0x20 != 0 {
        names.push("internal");
    }
    if flags & 0x10 != 0 {
        names.push("obsolete");
    }
    if flags & 0x0f != 0 {
        names.push("reserved");
    }
    names
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryInfo {
    pub percentage: u8,
    pub level_code: u8,
    pub status_code: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Keyboard,
    RemoteControl,
    Numpad,
    Mouse,
    Trackpad,
    Trackball,
    Presenter,
    Receiver,
    Headset,
    Webcam,
    SteeringWheel,
    Joystick,
    Gamepad,
    Dock,
    Speaker,
    Microphone,
    IlluminationLight,
    ProgrammableController,
    CarSimPedals,
    Adapter,
    Unknown(u8),
}

impl From<u8> for DeviceType {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Keyboard,
            1 => Self::RemoteControl,
            2 => Self::Numpad,
            3 => Self::Mouse,
            4 => Self::Trackpad,
            5 => Self::Trackball,
            6 => Self::Presenter,
            7 => Self::Receiver,
            8 => Self::Headset,
            9 => Self::Webcam,
            10 => Self::SteeringWheel,
            11 => Self::Joystick,
            12 => Self::Gamepad,
            13 => Self::Dock,
            14 => Self::Speaker,
            15 => Self::Microphone,
            16 => Self::IlluminationLight,
            17 => Self::ProgrammableController,
            18 => Self::CarSimPedals,
            19 => Self::Adapter,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for DeviceType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Keyboard => "keyboard",
            Self::RemoteControl => "remote control",
            Self::Numpad => "numpad",
            Self::Mouse => "mouse",
            Self::Trackpad => "trackpad",
            Self::Trackball => "trackball",
            Self::Presenter => "presenter",
            Self::Receiver => "receiver",
            Self::Headset => "headset",
            Self::Webcam => "webcam",
            Self::SteeringWheel => "steering wheel",
            Self::Joystick => "joystick",
            Self::Gamepad => "gamepad",
            Self::Dock => "dock",
            Self::Speaker => "speaker",
            Self::Microphone => "microphone",
            Self::IlluminationLight => "illumination light",
            Self::ProgrammableController => "programmable controller",
            Self::CarSimPedals => "car simulator pedals",
            Self::Adapter => "adapter",
            Self::Unknown(value) => return write!(formatter, "unknown (0x{value:02x})"),
        };
        formatter.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInformation {
    pub entity_count: u8,
    pub unit_id: [u8; 4],
    pub transport_flags: u8,
    pub model_ids: [u16; 3],
    pub extended_model_id: u8,
    pub capabilities: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareEntity {
    pub entity_type: u8,
    pub prefix: String,
    pub firmware_number: u8,
    pub revision: u8,
    pub build: u16,
    pub active: bool,
    pub transport_pid: u16,
    pub extra_version: [u8; 5],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceMetadata {
    pub name: String,
    pub device_type: DeviceType,
    pub information: DeviceInformation,
    pub firmware: Vec<FirmwareEntity>,
    pub serial_number: Option<String>,
}

impl BatteryInfo {
    pub fn status_name(self) -> &'static str {
        match self.status_code {
            0x00 => "discharging",
            0x01 => "recharging",
            0x02 => "almost full",
            0x03 => "full",
            0x04 => "slow recharge",
            0x05 => "invalid battery",
            0x06 => "thermal error",
            _ => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpiInfo {
    pub sensor: u8,
    pub current_x: u16,
    pub default_x: u16,
    pub current_y: Option<u16>,
    pub default_y: Option<u16>,
    pub lod: Option<LiftOffDistance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LiftOffDistance {
    Low = 0,
    Medium = 1,
    High = 2,
}

impl TryFrom<u8> for LiftOffDistance {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Low),
            1 => Ok(Self::Medium),
            2 => Ok(Self::High),
            _ => bail!("unknown lift-off distance value 0x{value:02x}"),
        }
    }
}

impl fmt::Display for LiftOffDistance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedDpiCapabilities {
    pub has_y: bool,
    pub has_lod: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollingRateInfo {
    pub current_hz: u16,
    pub supported_hz: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorLedInfo {
    pub zone_count: u8,
    pub nv_capabilities: u16,
    pub extended_capabilities: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorLedEffectInfo {
    pub index: u8,
    pub effect_id: u16,
    pub capabilities: u16,
    pub period_ms: u16,
}

impl ColorLedEffectInfo {
    pub fn effect_name(self) -> &'static str {
        color_led_effect_name(self.effect_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorLedZoneInfo {
    pub index: u8,
    pub location: u16,
    pub persistency_capabilities: u8,
    pub effects: Vec<ColorLedEffectInfo>,
    pub current_effect_index: Option<u8>,
    pub current_params: Option<[u8; 10]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorLedState {
    pub info: ColorLedInfo,
    pub software_control: Option<bool>,
    pub sync_events: Option<bool>,
    pub zones: Vec<ColorLedZoneInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl fmt::Display for RgbColor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "#{:02x}{:02x}{:02x}",
            self.red, self.green, self.blue
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLedEffect {
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

impl ColorLedEffect {
    fn effect_id(self) -> u16 {
        match self {
            Self::Disabled => 0x0000,
            Self::Fixed { .. } => 0x0001,
            Self::Cycling { .. } => 0x0003,
            Self::Breathing { .. } => 0x000a,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorLedEffectSettings {
    pub zone_index: u8,
    pub zone_effect_index: u8,
    pub params: [u8; 10],
}

fn color_led_effect_name(effect_id: u16) -> &'static str {
    match effect_id {
        0x0000 => "disabled",
        0x0001 => "fixed color",
        0x0002 => "legacy breathing",
        0x0003 => "cycling",
        0x0004 => "color wave",
        0x0005 => "starlight",
        0x0006 => "light on press",
        0x0007 => "audio visualizer",
        0x0008 => "boot-up",
        0x0009 => "demo",
        0x000a => "breathing",
        0x000b => "ripple",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    Wired = 0x00,
    GamingWireless = 0x01,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardProfilesInfo {
    pub mode: u8,
    pub active_profile: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardProfilesDescription {
    pub memory_model_id: u8,
    pub profile_format_id: u8,
    pub macro_format_id: u8,
    pub profile_count: u8,
    pub factory_profile_count: u8,
    pub button_count: u8,
    pub sector_count: u8,
    pub sector_size: u16,
    pub mechanical_layout: u8,
    pub various_info: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardProfileDirectoryEntry {
    pub sector: u16,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardDpiStage {
    pub x: u16,
    pub y: Option<u16>,
    pub lod: Option<LiftOffDistance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardButtonBinding {
    pub raw: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OnboardSpecialAction {
    NoAction = 0x00,
    TiltLeft = 0x01,
    TiltRight = 0x02,
    NextDpi = 0x03,
    PreviousDpi = 0x04,
    CycleDpi = 0x05,
    DefaultDpi = 0x06,
    DpiShift = 0x07,
    NextProfile = 0x08,
    PreviousProfile = 0x09,
    CycleProfile = 0x0a,
    GShift = 0x0b,
    BatteryIndicator = 0x0c,
    EnableProfile = 0x0d,
    PerformanceSwitch = 0x0e,
    ScrollDown = 0x10,
    ScrollUp = 0x11,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardButtonAction {
    Disabled,
    NoAction,
    MouseButtons(u16),
    Keyboard {
        modifiers: u8,
        usage: u8,
    },
    Consumer {
        usage: u16,
    },
    Special {
        action: OnboardSpecialAction,
        profile: u8,
    },
}

impl OnboardButtonBinding {
    pub fn from_action(action: OnboardButtonAction) -> Result<Self> {
        let raw = match action {
            OnboardButtonAction::Disabled => [0xff; 4],
            OnboardButtonAction::NoAction => [0x80, 0x00, 0x00, 0x00],
            OnboardButtonAction::MouseButtons(mask) => {
                if mask == 0 {
                    bail!("onboard mouse-button mask must not be zero");
                }
                let [high, low] = mask.to_be_bytes();
                [0x80, 0x01, high, low]
            }
            OnboardButtonAction::Keyboard { modifiers, usage } => [0x80, 0x02, modifiers, usage],
            OnboardButtonAction::Consumer { usage } => {
                let [high, low] = usage.to_be_bytes();
                [0x80, 0x03, high, low]
            }
            OnboardButtonAction::Special { action, profile } => [0x90, action as u8, 0x00, profile],
        };
        Ok(Self { raw })
    }

    pub fn description(self) -> String {
        match (self.raw[0], self.raw[1]) {
            (0xff, _) => "disabled".to_owned(),
            (0x80, 0x00) => "no action".to_owned(),
            (0x80, 0x01) => {
                let mask = u16::from_be_bytes([self.raw[2], self.raw[3]]);
                format!("mouse buttons 0x{mask:04x}")
            }
            (0x80, 0x02) => format!(
                "keyboard modifiers 0x{:02x} + usage 0x{:02x}",
                self.raw[2], self.raw[3]
            ),
            (0x80, 0x03) => {
                let usage = u16::from_be_bytes([self.raw[2], self.raw[3]]);
                format!("consumer usage 0x{usage:04x}")
            }
            (0x90, special) => special_button_name(special).to_owned(),
            (0x00, _) => format!(
                "macro sector 0x{:02x}, offset 0x{:02x}",
                self.raw[1], self.raw[3]
            ),
            _ => format!(
                "unknown {:02x} {:02x} {:02x} {:02x}",
                self.raw[0], self.raw[1], self.raw[2], self.raw[3]
            ),
        }
    }
}

fn special_button_name(code: u8) -> &'static str {
    match code {
        0x00 => "special: no action",
        0x01 => "tilt left",
        0x02 => "tilt right",
        0x03 => "next DPI",
        0x04 => "previous DPI",
        0x05 => "cycle DPI",
        0x06 => "default DPI",
        0x07 => "DPI shift",
        0x08 => "next profile",
        0x09 => "previous profile",
        0x0a => "cycle profile",
        0x0b => "G-Shift",
        0x0c => "battery indicator",
        0x0d => "enable profile",
        0x0e => "performance switch",
        0x0f => "host action",
        0x10 => "scroll down",
        0x11 => "scroll up",
        _ => "unknown special action",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardProfile {
    pub sector: u16,
    pub enabled: bool,
    pub crc_valid: bool,
    pub name: Option<String>,
    pub wired_hz: Option<u16>,
    pub wireless_hz: Option<u16>,
    pub default_dpi_index: u8,
    pub shifted_dpi_index: u8,
    pub dpi_stages: Vec<OnboardDpiStage>,
    pub bunny_hopping_timeout_ms: Option<u16>,
    pub power_save_timeout: u16,
    pub power_off_timeout: u16,
    pub buttons: Vec<OnboardButtonBinding>,
    /// Original sector bytes used for byte-preserving edits and stale-write checks.
    pub raw_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardProfileWriteProof {
    pub profile_number: usize,
    pub sector: u16,
    pub bytes_verified: usize,
    pub crc: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardProfileEditProof {
    pub profile_number: usize,
    pub sector: u16,
    pub original_name: Option<String>,
    pub temporary_name: String,
    pub bytes_restored: usize,
    pub original_crc: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardProfileEdit {
    Name(Option<String>),
    DpiStage {
        index: u8,
        x: u16,
        y: Option<u16>,
        lod: Option<LiftOffDistance>,
        make_default: bool,
        make_shift: bool,
    },
    DefaultDpiStage(u8),
    ShiftDpiStage(u8),
    PollingRate {
        connection: ConnectionType,
        hz: u16,
    },
    PowerTimeouts {
        save_seconds: u16,
        off_seconds: u16,
    },
    BunnyHopping {
        timeout_ms: Option<u16>,
    },
    Button {
        button: u8,
        action: OnboardButtonAction,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseButtonFilterInfo {
    pub button_count: u8,
    /// One entry per physical button. Zero suppresses the normal HID button;
    /// values 1..=16 select the host-visible mouse-button number.
    pub mapping: Vec<u8>,
}

/// Selects whether settings come from host software or the mouse's flash profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileControlMode {
    Onboard = 0x01,
    Host = 0x02,
}

/// Describes which side currently owns DPI, report rate, and button settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationSource {
    Host,
    Onboard { active_profile: Option<u16> },
    Unknown { raw_mode: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeStatusInfo {
    pub status0: u8,
    pub status1: u8,
    pub capabilities: u8,
    pub capabilities1: u8,
    pub surface_mode: Option<SurfaceMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingMode {
    Endurance,
    Performance,
}

impl fmt::Display for OperatingMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Endurance => "endurance",
            Self::Performance => "performance",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceMode {
    On,
    Automatic,
    Off,
}

impl SurfaceMode {
    const MASK: u8 = 0x03;

    fn status_bits(self) -> u8 {
        match self {
            Self::Automatic => 0x00,
            Self::On => 0x01,
            Self::Off => 0x02,
        }
    }
}

impl fmt::Display for SurfaceMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::On => "on",
            Self::Automatic => "auto",
            Self::Off => "off",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BunnyHoppingInfo {
    pub enabled: bool,
    /// Zero is reported while BHOP has no configured/active timeout.
    pub timeout_ms: u16,
}

impl ModeStatusInfo {
    pub fn performance(self) -> bool {
        self.status0 & 0x01 != 0
    }

    pub fn operating_mode(self) -> OperatingMode {
        if self.performance() {
            OperatingMode::Performance
        } else {
            OperatingMode::Endurance
        }
    }

    pub fn supports_software_operating_mode(self) -> bool {
        self.capabilities & 0x02 != 0
    }
}

impl OnboardProfilesInfo {
    pub fn mode_name(self) -> &'static str {
        match self.mode {
            0x01 => "enabled",
            0x02 => "disabled (host-controlled)",
            _ => "unknown",
        }
    }

    pub fn enabled(self) -> bool {
        self.mode == 0x01
    }

    pub fn configuration_source(self) -> ConfigurationSource {
        match self.mode {
            0x01 => ConfigurationSource::Onboard {
                active_profile: self.active_profile,
            },
            0x02 => ConfigurationSource::Host,
            raw_mode => ConfigurationSource::Unknown { raw_mode },
        }
    }
}

pub struct HidppSession {
    short_device: HidDevice,
    long_device: Option<HidDevice>,
    device_index: u8,
    timeout: Duration,
}

impl HidppSession {
    pub fn new(
        short_device: HidDevice,
        long_device: Option<HidDevice>,
        device_index: u8,
        timeout: Duration,
    ) -> Self {
        Self {
            short_device,
            long_device,
            device_index,
            timeout,
        }
    }

    pub fn protocol_version(&self) -> Result<ProtocolVersion> {
        const MARKER: u8 = 0x5a;
        let response = self.request(0x00, 0x10, &[0x00, 0x00, MARKER])?;

        if response.len() < 3 {
            bail!("short HID++ protocol-version reply: {}", hex(&response));
        }
        if response[2] != MARKER {
            bail!(
                "HID++ protocol-version marker mismatch: expected {MARKER:02x}, got {:02x}",
                response[2]
            );
        }

        Ok(ProtocolVersion {
            major: response[0],
            minor: response[1],
        })
    }

    pub fn feature(&self, feature_id: u16) -> Result<Option<FeatureInfo>> {
        let response = self.request(0x00, 0x00, &feature_id.to_be_bytes())?;
        if response.len() < 3 {
            bail!(
                "short HID++ GetFeature reply for 0x{feature_id:04x}: {}",
                hex(&response)
            );
        }

        if response[0] == 0 {
            return Ok(None);
        }

        Ok(Some(FeatureInfo {
            index: response[0],
            flags: response[1],
            version: response[2],
        }))
    }

    pub fn feature_set(&self) -> Result<Vec<FeatureSetEntry>> {
        let feature_set = self
            .feature(FEATURE_SET)?
            .context("device does not expose the HID++ Feature Set (0x0001)")?;
        let count_response = self.request(feature_set.index, 0x00, &[])?;
        let count = *count_response
            .first()
            .with_context(|| format!("short feature-count reply: {}", hex(&count_response)))?;
        let mut entries = Vec::with_capacity(usize::from(count) + 1);

        for index in 0..=count {
            let response = self.request(feature_set.index, 0x10, &[index])?;
            if response.len() < 4 {
                bail!(
                    "short Feature Set reply for index {index}: {}",
                    hex(&response)
                );
            }
            entries.push(FeatureSetEntry {
                index,
                feature_id: u16::from_be_bytes([response[0], response[1]]),
                flags: response[2],
                version: response[3],
            });
        }

        Ok(entries)
    }

    pub fn device_information(&self, feature: FeatureInfo) -> Result<DeviceInformation> {
        let response = self.request(feature.index, 0x00, &[])?;
        if response.len() < 15 {
            bail!("short device-information reply: {}", hex(&response));
        }
        Ok(DeviceInformation {
            entity_count: response[0],
            unit_id: response[1..5].try_into().expect("four-byte unit ID"),
            transport_flags: response[6],
            model_ids: [
                u16::from_be_bytes([response[7], response[8]]),
                u16::from_be_bytes([response[9], response[10]]),
                u16::from_be_bytes([response[11], response[12]]),
            ],
            extended_model_id: response[13],
            capabilities: response[14],
        })
    }

    pub fn firmware_entity(&self, feature: FeatureInfo, entity: u8) -> Result<FirmwareEntity> {
        let response = self.request(feature.index, 0x10, &[entity])?;
        if response.len() < 16 {
            bail!(
                "short firmware-entity reply for entity {entity}: {}",
                hex(&response)
            );
        }
        Ok(FirmwareEntity {
            entity_type: response[0],
            prefix: String::from_utf8_lossy(&response[1..4])
                .trim_end_matches('\0')
                .to_owned(),
            firmware_number: decode_bcd_byte(response[4]),
            revision: decode_bcd_byte(response[5]),
            build: decode_bcd_word(u16::from_be_bytes([response[6], response[7]])),
            active: response[8] & 0x01 != 0,
            transport_pid: u16::from_be_bytes([response[9], response[10]]),
            extra_version: response[11..16]
                .try_into()
                .expect("five-byte extra version"),
        })
    }

    pub fn device_serial_number(&self, feature: FeatureInfo) -> Result<String> {
        let response = self.request(feature.index, 0x20, &[])?;
        Ok(String::from_utf8_lossy(&response[..response.len().min(12)])
            .trim_end_matches('\0')
            .to_owned())
    }

    pub fn device_name(&self, feature: FeatureInfo) -> Result<String> {
        let count_response = self.request(feature.index, 0x00, &[])?;
        let count =
            usize::from(*count_response.first().with_context(|| {
                format!("short device-name count reply: {}", hex(&count_response))
            })?);
        let mut bytes = Vec::with_capacity(count);
        while bytes.len() < count {
            let offset =
                u8::try_from(bytes.len()).context("device name is longer than 255 bytes")?;
            let response = self.request(feature.index, 0x10, &[offset])?;
            if response.is_empty() {
                bail!("empty device-name chunk at offset {offset}");
            }
            let remaining = count - bytes.len();
            bytes.extend_from_slice(&response[..response.len().min(remaining)]);
        }
        Ok(String::from_utf8_lossy(&bytes)
            .trim_end_matches('\0')
            .to_owned())
    }

    pub fn device_type(&self, feature: FeatureInfo) -> Result<DeviceType> {
        let response = self.request(feature.index, 0x20, &[])?;
        response
            .first()
            .copied()
            .map(DeviceType::from)
            .with_context(|| format!("short device-type reply: {}", hex(&response)))
    }

    pub fn unified_battery(&self, feature: FeatureInfo) -> Result<BatteryInfo> {
        parse_unified_battery(&self.request(feature.index, 0x10, &[])?)
    }

    pub fn legacy_battery(&self, feature: FeatureInfo) -> Result<BatteryInfo> {
        parse_legacy_battery(&self.request(feature.index, 0x00, &[])?)
    }

    pub fn adjustable_dpi(&self, feature: FeatureInfo) -> Result<DpiInfo> {
        parse_adjustable_dpi(&self.request(feature.index, 0x20, &[])?)
    }

    pub fn extended_dpi_capabilities(
        &self,
        feature: FeatureInfo,
    ) -> Result<ExtendedDpiCapabilities> {
        parse_extended_dpi_capabilities(&self.request(feature.index, 0x10, &[0x00])?)
    }

    pub fn extended_adjustable_dpi(
        &self,
        feature: FeatureInfo,
        capabilities: ExtendedDpiCapabilities,
    ) -> Result<DpiInfo> {
        parse_extended_adjustable_dpi(&self.request(feature.index, 0x50, &[])?, capabilities)
    }

    pub fn supported_adjustable_dpi(&self, feature: FeatureInfo) -> Result<Vec<u16>> {
        // 0x2201 returns one unpaged payload: sensor index followed by DPI words.
        parse_adjustable_dpi_list_response(&self.request(feature.index, 0x10, &[0x00])?)
    }

    pub fn supported_extended_adjustable_dpi(&self, feature: FeatureInfo) -> Result<Vec<u16>> {
        self.read_dpi_list(feature, 0x20, 0)
    }

    pub fn set_adjustable_dpi(&self, feature: FeatureInfo, dpi: u16) -> Result<()> {
        let [high, low] = dpi.to_be_bytes();
        self.request(feature.index, 0x30, &[0x00, high, low])?;
        Ok(())
    }

    pub fn set_extended_adjustable_dpi(
        &self,
        feature: FeatureInfo,
        capabilities: ExtendedDpiCapabilities,
        dpi_x: u16,
        dpi_y: u16,
        lod: Option<LiftOffDistance>,
    ) -> Result<()> {
        let [x_high, x_low] = dpi_x.to_be_bytes();
        let [y_high, y_low] = dpi_y.to_be_bytes();
        let mut params = vec![0x00, x_high, x_low];
        if capabilities.has_y {
            params.extend_from_slice(&[y_high, y_low]);
        } else {
            params.extend_from_slice(&[0x00, 0x00]);
        }
        params.push(if capabilities.has_lod {
            lod.context("device supports LOD, but its current LOD could not be read")? as u8
        } else {
            0x00
        });
        self.request(feature.index, 0x60, &params)?;
        Ok(())
    }

    pub fn report_rate(&self, feature: FeatureInfo) -> Result<PollingRateInfo> {
        let flags = self.request(feature.index, 0x00, &[])?;
        let current = self.request(feature.index, 0x10, &[])?;
        parse_report_rate(&flags, &current)
    }

    pub fn set_report_rate(&self, feature: FeatureInfo, hz: u16) -> Result<()> {
        let period_ms = legacy_rate_code(hz)
            .with_context(|| format!("{hz} Hz cannot be represented by feature 0x8060"))?;
        self.request(feature.index, 0x20, &[period_ms])?;
        Ok(())
    }

    pub fn extended_report_rate(
        &self,
        feature: FeatureInfo,
        connection: ConnectionType,
    ) -> Result<PollingRateInfo> {
        let flags = self.request(feature.index, 0x00, &[connection as u8])?;
        let current = self.request(feature.index, 0x20, &[connection as u8])?;
        parse_extended_report_rate(&flags, &current)
    }

    pub fn actual_extended_report_rates(&self, feature: FeatureInfo) -> Result<Vec<u16>> {
        parse_extended_rate_flags(&self.request(feature.index, 0x10, &[])?)
    }

    pub fn set_extended_report_rate(&self, feature: FeatureInfo, hz: u16) -> Result<()> {
        let code = extended_rate_code(hz)
            .with_context(|| format!("{hz} Hz cannot be represented by feature 0x8061"))?;
        self.request(feature.index, 0x30, &[code])?;
        Ok(())
    }

    pub fn color_led_state(&self, feature: FeatureInfo) -> Result<ColorLedState> {
        let response = self.request(feature.index, 0x00, &[0x00, 0x00, 0x00])?;
        if response.len() < 5 {
            bail!("short Color LED Effects info reply: {}", hex(&response));
        }
        let info = ColorLedInfo {
            zone_count: response[0],
            nv_capabilities: u16::from_be_bytes([response[1], response[2]]),
            extended_capabilities: u16::from_be_bytes([response[3], response[4]]),
        };
        let software = self
            .request(feature.index, 0x70, &[0x00, 0x00, 0x00])
            .ok()
            .filter(|response| response.len() >= 2);
        let mut zones = Vec::with_capacity(usize::from(info.zone_count));
        for zone_index in 0..info.zone_count {
            let zone = self.request(feature.index, 0x10, &[zone_index, 0x00, 0x00])?;
            if zone.len() < 5 {
                bail!("short Color LED zone {zone_index} reply: {}", hex(&zone));
            }
            let effect_count = zone[3];
            let mut effects = Vec::with_capacity(usize::from(effect_count));
            for effect_index in 0..effect_count {
                let effect =
                    self.request(feature.index, 0x20, &[zone_index, effect_index, 0x00])?;
                if effect.len() < 8 {
                    bail!(
                        "short Color LED zone {zone_index} effect {effect_index} reply: {}",
                        hex(&effect)
                    );
                }
                effects.push(ColorLedEffectInfo {
                    index: effect[1],
                    effect_id: u16::from_be_bytes([effect[2], effect[3]]),
                    capabilities: u16::from_be_bytes([effect[4], effect[5]]),
                    period_ms: u16::from_be_bytes([effect[6], effect[7]]),
                });
            }

            let current = if info.extended_capabilities & 0x0001 != 0 {
                self.request(feature.index, 0xe0, &[zone_index, 0x00, 0x00])
                    .ok()
                    .filter(|response| response.len() >= 12)
            } else {
                None
            };
            let current_effect_index = current.as_ref().map(|response| response[1]);
            let current_params = current.map(|response| {
                response[2..12]
                    .try_into()
                    .expect("ten-byte Color LED effect parameters")
            });
            zones.push(ColorLedZoneInfo {
                index: zone[0],
                location: u16::from_be_bytes([zone[1], zone[2]]),
                persistency_capabilities: zone[4],
                effects,
                current_effect_index,
                current_params,
            });
        }
        Ok(ColorLedState {
            info,
            software_control: software.as_ref().map(|response| response[0] != 0),
            sync_events: software.as_ref().map(|response| response[1] != 0),
            zones,
        })
    }

    pub fn set_color_led_software_control(
        &self,
        feature: FeatureInfo,
        software_control: bool,
    ) -> Result<()> {
        self.request(
            feature.index,
            0x80,
            &[u8::from(software_control), 0x00, 0x00],
        )?;
        Ok(())
    }

    pub fn set_color_led_effect(
        &self,
        feature: FeatureInfo,
        zone_index: u8,
        zone_effect_index: u8,
        effect: ColorLedEffect,
    ) -> Result<()> {
        let params = encode_color_led_effect(zone_index, zone_effect_index, effect);
        self.request(feature.index, 0x30, &params)?;
        Ok(())
    }

    pub fn color_led_effect_settings(
        &self,
        feature: FeatureInfo,
        zone_index: u8,
    ) -> Result<ColorLedEffectSettings> {
        let response = self.request(feature.index, 0x90, &[zone_index, 0x00, 0x00])?;
        parse_color_led_effect_settings(&response).with_context(|| {
            format!(
                "invalid Color LED effect-settings reply for zone {zone_index}: {}",
                hex(&response)
            )
        })
    }

    pub fn onboard_profiles(&self, feature: FeatureInfo) -> Result<OnboardProfilesInfo> {
        let response = self.request(feature.index, 0x20, &[])?;
        let mode = *response
            .first()
            .with_context(|| format!("short onboard-profile mode reply: {}", hex(&response)))?;
        let active_profile = if mode == 0x01 {
            let active = self.request(feature.index, 0x40, &[])?;
            if active.len() < 2 {
                bail!("short active onboard-profile reply: {}", hex(&active));
            }
            Some(u16::from_be_bytes([active[0], active[1]]))
        } else {
            None
        };

        Ok(OnboardProfilesInfo {
            mode,
            active_profile,
        })
    }

    pub fn onboard_profiles_description(
        &self,
        feature: FeatureInfo,
    ) -> Result<OnboardProfilesDescription> {
        let response = self.request(feature.index, 0x00, &[])?;
        if response.len() < 11 {
            bail!(
                "short onboard-profile description reply: {}",
                hex(&response)
            );
        }

        Ok(OnboardProfilesDescription {
            memory_model_id: response[0],
            profile_format_id: response[1],
            macro_format_id: response[2],
            profile_count: response[3],
            factory_profile_count: response[4],
            button_count: response[5],
            sector_count: response[6],
            sector_size: u16::from_be_bytes([response[7], response[8]]),
            mechanical_layout: response[9],
            various_info: response[10],
        })
    }

    pub fn read_onboard_profile_sector(
        &self,
        feature: FeatureInfo,
        sector: u16,
        sector_size: u16,
    ) -> Result<Vec<u8>> {
        if sector_size < 16 {
            bail!("onboard-profile sector size {sector_size} is smaller than 16 bytes");
        }

        let sector_size = usize::from(sector_size);
        let mut data = vec![0_u8; sector_size];
        let mut offset = 0_usize;
        loop {
            let read_offset = offset.min(sector_size - 16);
            let read_offset_u16 = u16::try_from(read_offset)
                .context("onboard-profile sector offset does not fit in 16 bits")?;
            let mut params = [0_u8; 4];
            params[..2].copy_from_slice(&sector.to_be_bytes());
            params[2..].copy_from_slice(&read_offset_u16.to_be_bytes());
            let response = self.request(feature.index, 0x50, &params)?;
            if response.len() < 16 {
                bail!(
                    "short onboard-profile read at sector 0x{sector:04x}, offset {read_offset}: {}",
                    hex(&response)
                );
            }
            data[read_offset..read_offset + 16].copy_from_slice(&response[..16]);

            if read_offset + 16 >= sector_size {
                break;
            }
            offset += 16;
        }
        Ok(data)
    }

    pub fn write_onboard_profile_sector(
        &self,
        feature: FeatureInfo,
        sector: u16,
        data: &[u8],
    ) -> Result<()> {
        if sector > 0x00ff {
            bail!("refusing to write read-only onboard sector 0x{sector:04x}");
        }
        let length = u16::try_from(data.len())
            .context("onboard-profile sector data is larger than 65535 bytes")?;
        if length == 0 {
            bail!("refusing to write an empty onboard-profile sector");
        }

        let mut address = [0_u8; 6];
        address[..2].copy_from_slice(&sector.to_be_bytes());
        address[2..4].copy_from_slice(&0_u16.to_be_bytes());
        address[4..].copy_from_slice(&length.to_be_bytes());
        self.request(feature.index, 0x60, &address)?;

        for chunk in data.chunks(16) {
            self.request(feature.index, 0x70, chunk)?;
        }
        self.request(feature.index, 0x80, &[])?;
        Ok(())
    }

    pub fn mouse_button_filter(&self, feature: FeatureInfo) -> Result<MouseButtonFilterInfo> {
        let count_response = self.request(feature.index, 0x00, &[])?;
        let button_count = *count_response
            .first()
            .with_context(|| format!("short mouse-button count reply: {}", hex(&count_response)))?;
        let mapping_response = self.request(feature.index, 0x30, &[])?;
        if mapping_response.len() < usize::from(button_count) {
            bail!(
                "short mouse-button mapping reply: expected {button_count} bytes, got {}",
                mapping_response.len()
            );
        }
        Ok(MouseButtonFilterInfo {
            button_count,
            mapping: mapping_response[..usize::from(button_count)].to_vec(),
        })
    }

    pub fn set_mouse_button_filter(&self, feature: FeatureInfo, mapping: &[u8]) -> Result<()> {
        if mapping.len() > 16 {
            bail!("mouse-button mapping supports at most 16 physical buttons");
        }
        if let Some(invalid) = mapping.iter().find(|button| **button > 16) {
            bail!("invalid mapped mouse button {invalid}; expected 0..=16");
        }
        self.request(feature.index, 0x40, mapping)?;
        Ok(())
    }

    pub fn set_profile_control_mode(
        &self,
        feature: FeatureInfo,
        mode: ProfileControlMode,
    ) -> Result<()> {
        self.request(feature.index, 0x10, &[mode as u8])?;
        Ok(())
    }

    pub fn disable_onboard_profiles(&self, feature: FeatureInfo) -> Result<()> {
        self.set_profile_control_mode(feature, ProfileControlMode::Host)
    }

    /// Enables onboard control and selects an existing stored profile.
    ///
    /// This does not rewrite the profile contents in flash.
    pub fn activate_onboard_profile(&self, feature: FeatureInfo, profile: u16) -> Result<()> {
        if profile == 0 {
            bail!("onboard profile sector 0 is reserved for host-controlled mode");
        }
        self.set_profile_control_mode(feature, ProfileControlMode::Onboard)?;
        self.request(feature.index, 0x30, &profile.to_be_bytes())?;
        Ok(())
    }

    pub fn current_onboard_dpi_index(&self, feature: FeatureInfo) -> Result<u8> {
        let response = self.request(feature.index, 0xb0, &[])?;
        response
            .first()
            .copied()
            .with_context(|| format!("short current DPI-index reply: {}", hex(&response)))
    }

    pub fn set_current_onboard_dpi_index(&self, feature: FeatureInfo, index: u8) -> Result<()> {
        if index > 4 {
            bail!("onboard DPI stage must be in 0..=4");
        }
        self.request(feature.index, 0xc0, &[index])?;
        Ok(())
    }

    pub fn mode_status(&self, feature: FeatureInfo) -> Result<ModeStatusInfo> {
        let status = self.request(feature.index, 0x00, &[])?;
        let config = self.request(feature.index, 0x20, &[])?;
        parse_mode_status(&status, &config)
    }

    pub fn set_surface_mode(&self, feature: FeatureInfo, mode: SurfaceMode) -> Result<()> {
        // SetModeStatus requires desired bytes followed by per-byte changed-bit
        // masks. Bytes outside the masks must be zero; the device preserves them.
        // Four parameters intentionally select a HID++ long report.
        self.request(
            feature.index,
            0x10,
            &[0x00, mode.status_bits(), 0x00, SurfaceMode::MASK],
        )?;
        Ok(())
    }

    pub fn set_operating_mode(&self, feature: FeatureInfo, mode: OperatingMode) -> Result<()> {
        let status = match mode {
            OperatingMode::Endurance => 0x00,
            OperatingMode::Performance => 0x01,
        };
        self.request(feature.index, 0x10, &[status, 0x00, 0x01, 0x00])?;
        Ok(())
    }

    pub fn bunny_hopping(&self, feature: FeatureInfo) -> Result<BunnyHoppingInfo> {
        parse_bunny_hopping(&self.request(feature.index, 0x10, &[])?)
    }

    pub fn set_bunny_hopping(
        &self,
        feature: FeatureInfo,
        enabled: bool,
        timeout_ms: u16,
    ) -> Result<()> {
        let effective_timeout_ms = if enabled { timeout_ms } else { 0 };
        let timeout_units = u8::try_from(effective_timeout_ms / 10)
            .context("BHOP timeout cannot be represented in 10 ms units")?;
        self.request(feature.index, 0x20, &[timeout_units])?;
        Ok(())
    }

    fn read_dpi_list(&self, feature: FeatureInfo, function: u8, direction: u8) -> Result<Vec<u16>> {
        let mut encoded = Vec::new();
        let mut terminated = false;

        for page in 0_u8..=u8::MAX {
            let response = self.request(feature.index, function, &[0x00, direction, page])?;
            if response.len() < 5 {
                bail!("short DPI list reply: {}", hex(&response));
            }

            // The HID++ payload after the three-byte page header is 13 bytes long,
            // so a two-byte DPI value can straddle two replies. Preserve the raw
            // byte stream and only split it into pairs after all pages are joined.
            encoded.extend_from_slice(&response[3..]);
            if encoded.ends_with(&[0x00, 0x00]) {
                terminated = true;
                break;
            }
        }

        if !terminated {
            bail!("DPI list did not terminate after 256 pages");
        }
        parse_dpi_list(&encoded)
    }

    fn request(&self, feature_index: u8, function: u8, params: &[u8]) -> Result<Vec<u8>> {
        if function & 0x0f != 0 {
            bail!("HID++ function byte must have an empty software-ID nibble");
        }

        let function_and_software_id = function | SOFTWARE_ID;
        let request = encode_request(
            self.device_index,
            feature_index,
            function_and_software_id,
            params,
        )?;

        self.drain_input()?;
        let output_device = if request[0] == LONG_REPORT_ID {
            self.long_device.as_ref().unwrap_or(&self.short_device)
        } else {
            &self.short_device
        };
        let written = output_device
            .write(&request)
            .context("failed to write HID++ request")?;
        if written != request.len() {
            bail!(
                "partial HID++ write: wrote {written} of {} bytes",
                request.len()
            );
        }

        let deadline = Instant::now() + self.timeout;
        let mut input = [0_u8; 64];

        loop {
            let now = Instant::now();
            if now >= deadline {
                bail!(
                    "timed out waiting for HID++ reply to feature index 0x{feature_index:02x}, function 0x{function:02x}"
                );
            }

            let remaining_ms = deadline
                .saturating_duration_since(now)
                .as_millis()
                .clamp(1, i32::MAX as u128) as i32;
            let read = self.read_next(&mut input, remaining_ms)?;
            if read == 0 || read < 4 {
                continue;
            }

            let frame = &input[..read];
            if !matches!(frame[0], SHORT_REPORT_ID | LONG_REPORT_ID) {
                continue;
            }
            if frame[1] != self.device_index && frame[1] != (self.device_index ^ 0xff) {
                continue;
            }

            if frame.len() >= 6
                && frame[2] == 0xff
                && frame[3] == feature_index
                && frame[4] == function_and_software_id
            {
                bail!(
                    "device returned HID++ 2.0 error 0x{:02x} for request {}",
                    frame[5],
                    hex(&request)
                );
            }

            if frame[2] == feature_index && frame[3] == function_and_software_id {
                return Ok(frame[4..].to_vec());
            }
        }
    }

    fn drain_input(&self) -> Result<()> {
        let mut input = [0_u8; 64];
        drain_device(&self.short_device, &mut input)?;
        if let Some(long_device) = &self.long_device {
            drain_device(long_device, &mut input)?;
        }
        Ok(())
    }

    fn read_next(&self, input: &mut [u8], remaining_ms: i32) -> Result<usize> {
        let Some(long_device) = &self.long_device else {
            return self
                .short_device
                .read_timeout(input, remaining_ms)
                .context("failed to read HID++ reply");
        };

        let slice_ms = remaining_ms.min(10);
        let short_read = self
            .short_device
            .read_timeout(input, slice_ms)
            .context("failed to read short HID++ reply")?;
        if short_read != 0 {
            return Ok(short_read);
        }

        long_device
            .read_timeout(input, slice_ms)
            .context("failed to read long HID++ reply")
    }
}

fn encode_color_led_effect(
    zone_index: u8,
    zone_effect_index: u8,
    effect: ColorLedEffect,
) -> [u8; 13] {
    let mut params = [0_u8; 13];
    params[0] = zone_index;
    params[1] = zone_effect_index;
    match effect {
        ColorLedEffect::Disabled => {}
        ColorLedEffect::Fixed { color } => {
            params[2..5].copy_from_slice(&[color.red, color.green, color.blue]);
        }
        ColorLedEffect::Cycling {
            period_ms,
            brightness,
        } => {
            params[7..9].copy_from_slice(&period_ms.to_be_bytes());
            params[9] = if brightness == 100 { 0 } else { brightness };
        }
        ColorLedEffect::Breathing {
            color,
            period_ms,
            brightness,
        } => {
            params[2..5].copy_from_slice(&[color.red, color.green, color.blue]);
            params[5..7].copy_from_slice(&period_ms.to_be_bytes());
            params[7] = 0x00;
            params[8] = if brightness == 100 { 0 } else { brightness };
        }
    }
    // Volatile RAM only. Persistent LED writes require an explicit future API.
    params[12] = 0x00;
    params
}

fn parse_color_led_effect_settings(response: &[u8]) -> Result<ColorLedEffectSettings> {
    if response.len() < 12 {
        bail!("expected at least 12 bytes, received {}", response.len());
    }
    Ok(ColorLedEffectSettings {
        zone_index: response[0],
        zone_effect_index: response[1],
        params: response[2..12]
            .try_into()
            .expect("ten Color LED effect parameter bytes"),
    })
}

fn drain_device(device: &HidDevice, input: &mut [u8]) -> Result<()> {
    loop {
        let read = device
            .read_timeout(input, 0)
            .context("failed while draining stale HID input")?;
        if read == 0 {
            return Ok(());
        }
    }
}

fn decode_bcd_byte(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0f)
}

fn decode_bcd_word(value: u16) -> u16 {
    let thousands = (value >> 12) & 0x0f;
    let hundreds = (value >> 8) & 0x0f;
    let tens = (value >> 4) & 0x0f;
    let ones = value & 0x0f;
    thousands * 1000 + hundreds * 100 + tens * 10 + ones
}

fn encode_request(
    device_index: u8,
    feature_index: u8,
    function_and_software_id: u8,
    params: &[u8],
) -> Result<Vec<u8>> {
    let report_len = if params.len() <= 3 {
        SHORT_REPORT_LEN
    } else if params.len() <= 16 {
        LONG_REPORT_LEN
    } else {
        bail!("HID++ request has too many parameters: {}", params.len());
    };

    let mut report = vec![0_u8; report_len];
    report[0] = if report_len == SHORT_REPORT_LEN {
        SHORT_REPORT_ID
    } else {
        LONG_REPORT_ID
    };
    report[1] = device_index;
    report[2] = feature_index;
    report[3] = function_and_software_id;
    report[4..4 + params.len()].copy_from_slice(params);
    Ok(report)
}

fn parse_unified_battery(response: &[u8]) -> Result<BatteryInfo> {
    if response.len() < 4 {
        bail!("short Unified Battery reply: {}", hex(response));
    }

    Ok(BatteryInfo {
        percentage: response[0],
        level_code: response[1],
        status_code: response[2],
    })
}

fn parse_legacy_battery(response: &[u8]) -> Result<BatteryInfo> {
    if response.len() < 3 {
        bail!("short Battery Status reply: {}", hex(response));
    }

    Ok(BatteryInfo {
        percentage: response[0],
        level_code: response[1],
        status_code: response[2],
    })
}

fn parse_adjustable_dpi(response: &[u8]) -> Result<DpiInfo> {
    if response.len() < 5 {
        bail!("short Adjustable DPI reply: {}", hex(response));
    }

    let reported = u16::from_be_bytes([response[1], response[2]]);
    let default = u16::from_be_bytes([response[3], response[4]]);
    Ok(DpiInfo {
        sensor: response[0],
        current_x: if reported == 0 { default } else { reported },
        default_x: default,
        current_y: None,
        default_y: None,
        lod: None,
    })
}

fn parse_extended_dpi_capabilities(response: &[u8]) -> Result<ExtendedDpiCapabilities> {
    if response.len() < 3 {
        bail!(
            "short Extended Adjustable DPI capabilities reply: {}",
            hex(response)
        );
    }

    Ok(ExtendedDpiCapabilities {
        has_y: response[2] & 0x01 != 0,
        has_lod: response[2] & 0x02 != 0,
    })
}

fn parse_extended_adjustable_dpi(
    response: &[u8],
    capabilities: ExtendedDpiCapabilities,
) -> Result<DpiInfo> {
    if response.len() < 5 {
        bail!("short Extended Adjustable DPI reply: {}", hex(response));
    }

    let reported_x = u16::from_be_bytes([response[1], response[2]]);
    let default_x = u16::from_be_bytes([response[3], response[4]]);
    let (current_y, default_y) = if capabilities.has_y {
        if response.len() < 9 {
            bail!(
                "short Extended Adjustable DPI Y-axis reply: {}",
                hex(response)
            );
        }
        let reported_y = u16::from_be_bytes([response[5], response[6]]);
        let default_y = u16::from_be_bytes([response[7], response[8]]);
        let effective_y = if reported_y == 0 {
            default_y
        } else {
            reported_y
        };
        (Some(effective_y), Some(default_y))
    } else {
        (None, None)
    };

    Ok(DpiInfo {
        sensor: response[0],
        current_x: if reported_x == 0 {
            default_x
        } else {
            reported_x
        },
        default_x,
        current_y,
        default_y,
        lod: if capabilities.has_lod {
            Some(LiftOffDistance::try_from(
                *response
                    .get(9)
                    .context("short Extended Adjustable DPI LOD reply")?,
            )?)
        } else {
            None
        },
    })
}

fn parse_mode_status(status: &[u8], config: &[u8]) -> Result<ModeStatusInfo> {
    if status.len() < 2 {
        bail!("short mode-status reply: {}", hex(status));
    }
    if config.len() < 2 {
        bail!("short mode-status config reply: {}", hex(config));
    }

    let surface_mode = if config[1] & 0x0c != 0 {
        Some(match status[1] & SurfaceMode::MASK {
            0x00 => SurfaceMode::Automatic,
            0x01 => SurfaceMode::On,
            0x02 => SurfaceMode::Off,
            invalid => bail!(
                "invalid mode-status surface bits 0x{invalid:02x} in reply {}",
                hex(status)
            ),
        })
    } else {
        None
    };

    Ok(ModeStatusInfo {
        status0: status[0],
        status1: status[1],
        capabilities: config[0],
        capabilities1: config[1],
        surface_mode,
    })
}

fn parse_bunny_hopping(response: &[u8]) -> Result<BunnyHoppingInfo> {
    let timeout_units = *response
        .first()
        .with_context(|| format!("short BHOP timeout reply: {}", hex(response)))?;
    // 0xff is the firmware's unset/inherited sentinel while onboard control is
    // active. It cannot be produced by the supported 100..=1000 ms setter.
    if timeout_units == 0xff {
        return Ok(BunnyHoppingInfo {
            enabled: false,
            timeout_ms: 0,
        });
    }
    let timeout_ms = u16::from(timeout_units) * 10;
    Ok(BunnyHoppingInfo {
        enabled: timeout_units != 0,
        timeout_ms,
    })
}

fn parse_dpi_list(encoded: &[u8]) -> Result<Vec<u16>> {
    let mut values = Vec::new();
    let mut pairs = encoded.chunks_exact(2);

    while let Some(pair) = pairs.next() {
        let value = u16::from_be_bytes([pair[0], pair[1]]);
        if value == 0 {
            break;
        }

        if value >> 13 == 0b111 {
            let step = value & 0x1fff;
            let end_pair = pairs.next().context("DPI range marker has no end value")?;
            let end = u16::from_be_bytes([end_pair[0], end_pair[1]]);
            let start = values
                .last()
                .copied()
                .context("DPI range marker appeared before its starting value")?;
            if step == 0 || end <= start {
                bail!("invalid DPI range {start}..={end} with step {step}");
            }

            let mut dpi = start
                .checked_add(step)
                .context("DPI range overflowed u16")?;
            while dpi <= end {
                values.push(dpi);
                dpi = match dpi.checked_add(step) {
                    Some(next) => next,
                    None => break,
                };
            }
        } else {
            values.push(value);
        }
    }

    if values.is_empty() {
        bail!("device returned an empty DPI list");
    }
    Ok(values)
}

fn parse_adjustable_dpi_list_response(response: &[u8]) -> Result<Vec<u16>> {
    if response.len() < 3 {
        bail!("short Adjustable DPI list reply: {}", hex(response));
    }
    parse_dpi_list(&response[1..])
}

fn parse_report_rate(flags: &[u8], current: &[u8]) -> Result<PollingRateInfo> {
    let flags = *flags
        .first()
        .context("empty Report Rate capabilities reply")?;
    let period_ms = *current
        .first()
        .context("empty Report Rate current-value reply")?;
    let current_hz = legacy_rate_hz(period_ms)
        .with_context(|| format!("invalid Report Rate period {period_ms} ms"))?;
    let supported_hz = normalize_supported_hz(
        (1_u8..=8)
            .filter(|period| flags & (1 << (period - 1)) != 0)
            .filter_map(legacy_rate_hz)
            .collect(),
    );

    Ok(PollingRateInfo {
        current_hz,
        supported_hz,
    })
}

fn parse_extended_report_rate(flags: &[u8], current: &[u8]) -> Result<PollingRateInfo> {
    let code = *current
        .first()
        .context("empty Extended Report Rate current-value reply")?;
    let current_hz = extended_rate_hz(code)
        .with_context(|| format!("invalid Extended Report Rate code {code}"))?;
    let supported_hz = parse_extended_rate_flags(flags)?;

    Ok(PollingRateInfo {
        current_hz,
        supported_hz,
    })
}

fn parse_extended_rate_flags(flags: &[u8]) -> Result<Vec<u16>> {
    if flags.len() < 2 {
        bail!(
            "short Extended Report Rate capabilities reply: {}",
            hex(flags)
        );
    }
    let flags = u16::from_be_bytes([flags[0], flags[1]]);
    let supported_hz = normalize_supported_hz(
        (0_u8..=6)
            .filter(|code| flags & (1 << code) != 0)
            .filter_map(extended_rate_hz)
            .collect(),
    );
    Ok(supported_hz)
}

/// HID++ features encode rates in different orders. Keep the public model
/// deterministic for every caller, including the future settings UI.
fn normalize_supported_hz(mut rates: Vec<u16>) -> Vec<u16> {
    rates.sort_unstable();
    rates.dedup();
    rates
}

fn legacy_rate_hz(period_ms: u8) -> Option<u16> {
    (1..=8)
        .contains(&period_ms)
        .then(|| 1000 / u16::from(period_ms))
}

fn legacy_rate_code(hz: u16) -> Option<u8> {
    (1_u8..=8).find(|period| legacy_rate_hz(*period) == Some(hz))
}

fn extended_rate_hz(code: u8) -> Option<u16> {
    const HZ_BY_CODE: [u16; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];
    HZ_BY_CODE.get(usize::from(code)).copied()
}

fn extended_rate_code(hz: u16) -> Option<u8> {
    (0_u8..=6).find(|code| extended_rate_hz(*code) == Some(hz))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_short_get_feature_request() {
        let report = encode_request(1, 0, 0x0c, &[0x10, 0x04]).unwrap();
        assert_eq!(report, [0x10, 0x01, 0x00, 0x0c, 0x10, 0x04, 0x00]);
    }

    #[test]
    fn hidden_flags_block_even_known_user_features() {
        let descriptor = describe_feature(FEATURE_COLOR_LED_EFFECTS, 0x60);
        assert_eq!(descriptor.audience, FeatureAudience::Internal);
        assert_eq!(descriptor.support, FeatureSupport::IntentionallyBlocked);
    }

    #[test]
    fn parses_unified_battery() {
        let battery = parse_unified_battery(&[94, 8, 0, 0]).unwrap();
        assert_eq!(battery.percentage, 94);
        assert_eq!(battery.status_name(), "discharging");
    }

    #[test]
    fn parses_adjustable_dpi_default_fallback() {
        let dpi = parse_adjustable_dpi(&[0, 0, 0, 0x06, 0x40]).unwrap();
        assert_eq!(dpi.current_x, 1600);
        assert_eq!(dpi.default_x, 1600);
    }

    #[test]
    fn parses_extended_dpi_capabilities() {
        let capabilities = parse_extended_dpi_capabilities(&[0, 0, 0b11]).unwrap();
        assert!(capabilities.has_y);
        assert!(capabilities.has_lod);
    }

    #[test]
    fn parses_extended_adjustable_dpi() {
        let capabilities = ExtendedDpiCapabilities {
            has_y: true,
            has_lod: true,
        };
        let dpi = parse_extended_adjustable_dpi(
            &[0, 0x03, 0x20, 0x06, 0x40, 0x03, 0x20, 0x06, 0x40, 1],
            capabilities,
        )
        .unwrap();
        assert_eq!(dpi.current_x, 800);
        assert_eq!(dpi.current_y, Some(800));
        assert_eq!(dpi.lod, Some(LiftOffDistance::Medium));
    }

    #[test]
    fn parses_mode_status_surface_bits_without_losing_other_status() {
        let automatic = parse_mode_status(&[0x00, 0x00], &[0x00, 0x0c]).unwrap();
        assert_eq!(automatic.surface_mode, Some(SurfaceMode::Automatic));
        assert_eq!(automatic.status1, 0x00);
        assert_eq!(automatic.capabilities1, 0x0c);

        assert_eq!(
            parse_mode_status(&[0x00, 0x01], &[0x00, 0x0c])
                .unwrap()
                .surface_mode,
            Some(SurfaceMode::On)
        );
        assert_eq!(
            parse_mode_status(&[0x00, 0x02], &[0x00, 0x0c])
                .unwrap()
                .surface_mode,
            Some(SurfaceMode::Off)
        );
        assert!(parse_mode_status(&[0x00, 0x03], &[0x00, 0x0c]).is_err());

        let g305 = parse_mode_status(&[0x01, 0x00], &[0x02, 0x00]).unwrap();
        assert_eq!(g305.surface_mode, None);
        assert_eq!(g305.operating_mode(), OperatingMode::Performance);
        assert!(g305.supports_software_operating_mode());
    }

    #[test]
    fn encodes_volatile_legacy_color_led_effects() {
        let fixed = encode_color_led_effect(
            0,
            1,
            ColorLedEffect::Fixed {
                color: RgbColor {
                    red: 0x12,
                    green: 0x34,
                    blue: 0x56,
                },
            },
        );
        assert_eq!(fixed, [0, 1, 0x12, 0x34, 0x56, 0, 0, 0, 0, 0, 0, 0, 0]);

        let cycling = encode_color_led_effect(
            0,
            2,
            ColorLedEffect::Cycling {
                period_ms: 1000,
                brightness: 100,
            },
        );
        assert_eq!(cycling, [0, 2, 0, 0, 0, 0, 0, 0x03, 0xe8, 0, 0, 0, 0]);

        let breathing = encode_color_led_effect(
            0,
            3,
            ColorLedEffect::Breathing {
                color: RgbColor {
                    red: 0xaa,
                    green: 0xbb,
                    blue: 0xcc,
                },
                period_ms: 500,
                brightness: 40,
            },
        );
        assert_eq!(
            breathing,
            [0, 3, 0xaa, 0xbb, 0xcc, 0x01, 0xf4, 0, 40, 0, 0, 0, 0]
        );
    }

    #[test]
    fn parses_color_led_effect_settings_with_effect_index() {
        let settings = parse_color_led_effect_settings(&[
            0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xe8, 0x00, 0x00, 0x00,
        ])
        .unwrap();
        assert_eq!(settings.zone_index, 0);
        assert_eq!(settings.zone_effect_index, 2);
        assert_eq!(settings.params, [0, 0, 0, 0, 0, 0x03, 0xe8, 0, 0, 0]);
    }

    #[test]
    fn parses_bunny_hopping_state() {
        assert_eq!(
            parse_bunny_hopping(&[40]).unwrap(),
            BunnyHoppingInfo {
                enabled: true,
                timeout_ms: 400
            }
        );
        assert_eq!(
            parse_bunny_hopping(&[0]).unwrap(),
            BunnyHoppingInfo {
                enabled: false,
                timeout_ms: 0
            }
        );
        assert_eq!(
            parse_bunny_hopping(&[0xff]).unwrap(),
            BunnyHoppingInfo {
                enabled: false,
                timeout_ms: 0
            }
        );
        assert!(parse_bunny_hopping(&[]).is_err());
    }

    #[test]
    fn expands_compressed_dpi_list() {
        let values = parse_dpi_list(&[0x00, 0x64, 0xe0, 0x32, 0x01, 0x2c, 0x00, 0x00]).unwrap();
        assert_eq!(values, [100, 150, 200, 250, 300]);
    }

    #[test]
    fn parses_legacy_adjustable_dpi_response_header() {
        let values = parse_adjustable_dpi_list_response(&[
            0x00, 0x00, 0xc8, 0xe0, 0x32, 0x7d, 0x00, 0x00, 0x00,
        ])
        .unwrap();
        assert_eq!(values.first(), Some(&200));
        assert_eq!(values.last(), Some(&32000));
        assert_eq!(values.len(), 637);
    }

    #[test]
    fn parses_extended_report_rate() {
        let rate = parse_extended_report_rate(&[0x00, 0x7f], &[0x05]).unwrap();
        assert_eq!(rate.current_hz, 4000);
        assert_eq!(rate.supported_hz, [125, 250, 500, 1000, 2000, 4000, 8000]);
    }

    #[test]
    fn parses_legacy_report_rate() {
        let rate = parse_report_rate(&[0b1000_1011], &[0x01]).unwrap();
        assert_eq!(rate.current_hz, 1000);
        assert_eq!(rate.supported_hz, [125, 250, 500, 1000]);
    }

    #[test]
    fn distinguishes_host_and_onboard_configuration_sources() {
        let host = OnboardProfilesInfo {
            mode: 0x02,
            active_profile: None,
        };
        assert_eq!(host.configuration_source(), ConfigurationSource::Host);

        let onboard = OnboardProfilesInfo {
            mode: 0x01,
            active_profile: Some(0x0100),
        };
        assert_eq!(
            onboard.configuration_source(),
            ConfigurationSource::Onboard {
                active_profile: Some(0x0100)
            }
        );
    }
}
