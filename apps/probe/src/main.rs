use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use hidapi::{DeviceInfo, HidApi};

use open_hub_core::{
    ColorLedEffect, ConfigurationSource, ConnectionType, DeviceConnection, DeviceManager,
    HidppSession, LiftOffDistance, MouseDevice, OnboardButtonAction, OnboardProfileEdit,
    OnboardSpecialAction, OperatingMode, PollingRateCapabilities, PollingRateSettings, RgbColor,
    SurfaceMode, describe_feature, feature_flag_names,
};

const LOGITECH_VENDOR_ID: u16 = 0x046d;

#[derive(Debug, Parser)]
#[command(name = "open-hub-probe")]
#[command(about = "Logitech HID/HID++ diagnostic and configuration utility")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List HID interfaces visible through HIDAPI.
    List {
        /// Include devices from vendors other than Logitech.
        #[arg(long)]
        all: bool,
    },

    /// Automatically discover and inspect supported Logitech HID++ mice.
    Devices,

    /// Run read-only HID++ battery, DPI, and polling-rate queries.
    Probe {
        #[command(flatten)]
        device: DeviceArgs,
    },

    /// Dump the HID++ 2.0 feature table for protocol research.
    Features {
        #[command(flatten)]
        device: DeviceArgs,
    },

    /// Decode stored onboard profiles without changing flash or active settings.
    Profiles {
        #[command(flatten)]
        device: DeviceArgs,
    },

    /// Write identical data to a disabled profile and verify every flash byte.
    ProveProfileWrite {
        #[command(flatten)]
        device: DeviceArgs,

        /// One-based profile number as shown by the `profiles` command.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..))]
        profile: u8,

        /// Required acknowledgement that this command writes onboard flash.
        #[arg(long)]
        confirm_flash_write: bool,
    },

    /// Temporarily rename a disabled profile, verify it, then restore exact bytes.
    ProveProfileNameEdit {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,
        #[arg(long)]
        temporary_name: String,
    },

    /// Change an onboard profile name and verify its flash contents.
    SetProfileName {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,

        /// UTF-16 profile name (up to 24 code units).
        #[arg(
            long,
            conflicts_with = "clear_name",
            required_unless_present = "clear_name"
        )]
        name: Option<String>,

        /// Restore the firmware's default/unnamed profile label.
        #[arg(long, conflicts_with = "name")]
        clear_name: bool,
    },

    /// Change one onboard DPI stage and optionally select it as default/shift.
    SetProfileDpi {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,

        /// Zero-based DPI stage in 0..=4.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
        stage: u8,
        #[arg(long)]
        dpi: u16,
        /// Y-axis DPI; defaults to X on extended profiles.
        #[arg(long)]
        dpi_y: Option<u16>,
        /// LOD for extended profiles; omitted to preserve the current value.
        #[arg(long, value_enum)]
        lod: Option<LiftOffDistanceChoice>,
        #[arg(long)]
        make_default: bool,
        #[arg(long)]
        make_shift: bool,
    },

    /// Change a polling rate stored in an onboard profile.
    SetProfilePollingRate {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,
        #[arg(long)]
        hz: u16,
        #[arg(long, value_enum, default_value_t = PollingConnection::Wireless)]
        connection: PollingConnection,
    },

    /// Change profile power-save and power-off timeouts in seconds.
    SetProfilePowerTimeouts {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,
        #[arg(long)]
        save_seconds: u16,
        #[arg(long)]
        off_seconds: u16,
    },

    /// Change BHOP stored in a format 0x07 onboard profile.
    SetProfileBhop {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,
        #[arg(long, value_enum)]
        state: ToggleChoice,
        /// Double-scroll timeout in milliseconds (100..=1000, step 100).
        #[arg(long, default_value_t = 100)]
        window_ms: u16,
    },

    /// Change one normal onboard button assignment; macro creation is unsupported.
    SetProfileButton {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,
        /// One-based physical button number.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=16))]
        button: u8,
        #[command(subcommand)]
        action: ProfileButtonCommand,
    },

    /// Enable or disable a stored onboard profile safely.
    SetProfileEnabled {
        #[command(flatten)]
        device: DeviceArgs,
        #[command(flatten)]
        target: ProfileTargetArgs,
        #[arg(long, value_enum)]
        state: ToggleChoice,
    },

    /// Set both sensor axes to a validated DPI value and verify it.
    SetDpi {
        #[command(flatten)]
        device: DeviceArgs,

        /// Desired DPI; must be advertised by the mouse.
        #[arg(long)]
        dpi: u16,
    },

    /// Select a DPI stage from the active onboard profile and verify it.
    SetDpiStage {
        #[command(flatten)]
        device: DeviceArgs,

        /// Zero-based stage index in the range 0..=4.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
        stage: u8,
    },

    /// Set the sensor lift-off distance and verify it.
    SetLiftOffDistance {
        #[command(flatten)]
        device: DeviceArgs,

        /// Desired lift-off distance.
        #[arg(long, value_enum)]
        lod: LiftOffDistanceChoice,
    },

    /// Set Gaming Surface Mode and verify it.
    SetSurfaceMode {
        #[command(flatten)]
        device: DeviceArgs,

        /// Surface behavior: on, auto, or off.
        #[arg(long, value_enum)]
        mode: SurfaceModeChoice,
    },

    /// Set performance or endurance operating mode and verify it.
    SetOperatingMode {
        #[command(flatten)]
        device: DeviceArgs,
        #[arg(long, value_enum)]
        mode: OperatingModeChoice,
    },

    /// Apply a volatile Color LED effect under software control.
    SetLighting {
        #[command(flatten)]
        device: DeviceArgs,

        /// Zero-based lighting zone; the G305 exposes zone 0.
        #[arg(long, default_value_t = 0)]
        zone: u8,

        #[command(subcommand)]
        effect: LightingCommand,
    },

    /// Return Color LED control to the mouse firmware.
    UseFirmwareLighting {
        #[command(flatten)]
        device: DeviceArgs,
    },

    /// Configure the BHOP scroll-wheel helper and verify it.
    #[command(name = "set-bhop", visible_alias = "set-bunny-hopping")]
    SetBunnyHopping {
        #[command(flatten)]
        device: DeviceArgs,

        /// Whether BHOP should be on or off.
        #[arg(long, value_enum)]
        state: ToggleChoice,

        /// Double-scroll timeout in milliseconds (100..=1000, step 100).
        #[arg(long, default_value_t = 100)]
        window_ms: u16,
    },

    /// Set a validated polling rate in Hz and verify it.
    SetPollingRate {
        #[command(flatten)]
        device: DeviceArgs,

        /// Desired polling rate, for example 1000, 2000, 4000, or 8000.
        #[arg(long)]
        hz: u16,

        /// Connection whose value is validated and read back.
        #[arg(long, value_enum, default_value_t = PollingConnection::Wireless)]
        connection: PollingConnection,

        /// Switch to host control if an onboard profile is active; stored profiles are not erased.
        #[arg(long)]
        disable_onboard_profiles: bool,
    },

    /// Switch to host/local settings without erasing stored profiles.
    UseHostSettings {
        #[command(flatten)]
        device: DeviceArgs,
    },

    /// Activate an existing onboard profile sector without rewriting it.
    UseOnboardProfile {
        #[command(flatten)]
        device: DeviceArgs,

        /// Profile sector, for example 1 or 0x0101.
        #[arg(long, value_parser = parse_u16)]
        profile: u16,
    },
}

#[derive(Debug, Clone, Copy, Args)]
struct DeviceArgs {
    /// Zero-based list index or USB ID such as 046d:c54d.
    #[arg(long)]
    index: DeviceSelector,

    /// HID++ device index (1 for most receivers; ff for direct USB).
    #[arg(long, value_parser = parse_u8)]
    device_index: Option<u8>,

    /// Maximum time to wait for each reply.
    #[arg(long, default_value_t = 1500)]
    timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, Args)]
struct ProfileTargetArgs {
    /// One-based profile number shown by the `profiles` command.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..))]
    profile: u8,

    /// Required acknowledgement that this command rewrites onboard flash.
    #[arg(long)]
    confirm_flash_write: bool,
}

#[derive(Debug, Clone, Copy)]
enum DeviceSelector {
    ListIndex(usize),
    UsbId { vendor_id: u16, product_id: u16 },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PollingConnection {
    Wired,
    Wireless,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LiftOffDistanceChoice {
    Low,
    Medium,
    High,
}

impl From<LiftOffDistanceChoice> for LiftOffDistance {
    fn from(value: LiftOffDistanceChoice) -> Self {
        match value {
            LiftOffDistanceChoice::Low => Self::Low,
            LiftOffDistanceChoice::Medium => Self::Medium,
            LiftOffDistanceChoice::High => Self::High,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SurfaceModeChoice {
    On,
    Auto,
    Off,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OperatingModeChoice {
    Performance,
    Endurance,
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum LightingCommand {
    Off,
    Fixed {
        /// RGB color as RRGGBB or #RRGGBB.
        #[arg(long, value_parser = parse_rgb)]
        color: RgbColor,
    },
    Cycling {
        #[arg(long, default_value_t = 1000)]
        period_ms: u16,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(1..=100))]
        brightness: u8,
    },
    Breathing {
        /// RGB color as RRGGBB or #RRGGBB.
        #[arg(long, value_parser = parse_rgb)]
        color: RgbColor,
        #[arg(long, default_value_t = 1000)]
        period_ms: u16,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(1..=100))]
        brightness: u8,
    },
}

impl LightingCommand {
    fn into_effect(self) -> ColorLedEffect {
        match self {
            Self::Off => ColorLedEffect::Disabled,
            Self::Fixed { color } => ColorLedEffect::Fixed { color },
            Self::Cycling {
                period_ms,
                brightness,
            } => ColorLedEffect::Cycling {
                period_ms,
                brightness,
            },
            Self::Breathing {
                color,
                period_ms,
                brightness,
            } => ColorLedEffect::Breathing {
                color,
                period_ms,
                brightness,
            },
        }
    }
}

impl From<OperatingModeChoice> for OperatingMode {
    fn from(value: OperatingModeChoice) -> Self {
        match value {
            OperatingModeChoice::Performance => Self::Performance,
            OperatingModeChoice::Endurance => Self::Endurance,
        }
    }
}

impl From<SurfaceModeChoice> for SurfaceMode {
    fn from(value: SurfaceModeChoice) -> Self {
        match value {
            SurfaceModeChoice::On => Self::On,
            SurfaceModeChoice::Auto => Self::Automatic,
            SurfaceModeChoice::Off => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ToggleChoice {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum ProfileButtonCommand {
    Disabled,
    NoAction,
    Mouse {
        /// Host-visible mouse button number in 1..=16.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=16))]
        mouse_button: u8,
    },
    Keyboard {
        /// USB HID keyboard modifier mask, decimal or hexadecimal.
        #[arg(long, value_parser = parse_u8)]
        modifiers: u8,
        /// USB HID keyboard usage, decimal or hexadecimal.
        #[arg(long, value_parser = parse_u8)]
        usage: u8,
    },
    Consumer {
        /// USB HID consumer-page usage, decimal or hexadecimal.
        #[arg(long, value_parser = parse_u16)]
        usage: u16,
    },
    Special {
        #[arg(long, value_enum)]
        action: SpecialButtonChoice,
        /// Target profile for profile-related actions; otherwise leave as zero.
        #[arg(long, default_value_t = 0)]
        target_profile: u8,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SpecialButtonChoice {
    NoAction,
    TiltLeft,
    TiltRight,
    NextDpi,
    PreviousDpi,
    CycleDpi,
    DefaultDpi,
    DpiShift,
    NextProfile,
    PreviousProfile,
    CycleProfile,
    GShift,
    BatteryIndicator,
    EnableProfile,
    PerformanceSwitch,
    ScrollDown,
    ScrollUp,
}

impl ToggleChoice {
    fn enabled(self) -> bool {
        matches!(self, Self::On)
    }
}

impl ProfileButtonCommand {
    fn into_action(self) -> OnboardButtonAction {
        match self {
            Self::Disabled => OnboardButtonAction::Disabled,
            Self::NoAction => OnboardButtonAction::NoAction,
            Self::Mouse { mouse_button } => {
                OnboardButtonAction::MouseButtons(1_u16 << (mouse_button - 1))
            }
            Self::Keyboard { modifiers, usage } => {
                OnboardButtonAction::Keyboard { modifiers, usage }
            }
            Self::Consumer { usage } => OnboardButtonAction::Consumer { usage },
            Self::Special {
                action,
                target_profile,
            } => OnboardButtonAction::Special {
                action: action.into(),
                profile: target_profile,
            },
        }
    }
}

impl From<SpecialButtonChoice> for OnboardSpecialAction {
    fn from(value: SpecialButtonChoice) -> Self {
        match value {
            SpecialButtonChoice::NoAction => Self::NoAction,
            SpecialButtonChoice::TiltLeft => Self::TiltLeft,
            SpecialButtonChoice::TiltRight => Self::TiltRight,
            SpecialButtonChoice::NextDpi => Self::NextDpi,
            SpecialButtonChoice::PreviousDpi => Self::PreviousDpi,
            SpecialButtonChoice::CycleDpi => Self::CycleDpi,
            SpecialButtonChoice::DefaultDpi => Self::DefaultDpi,
            SpecialButtonChoice::DpiShift => Self::DpiShift,
            SpecialButtonChoice::NextProfile => Self::NextProfile,
            SpecialButtonChoice::PreviousProfile => Self::PreviousProfile,
            SpecialButtonChoice::CycleProfile => Self::CycleProfile,
            SpecialButtonChoice::GShift => Self::GShift,
            SpecialButtonChoice::BatteryIndicator => Self::BatteryIndicator,
            SpecialButtonChoice::EnableProfile => Self::EnableProfile,
            SpecialButtonChoice::PerformanceSwitch => Self::PerformanceSwitch,
            SpecialButtonChoice::ScrollDown => Self::ScrollDown,
            SpecialButtonChoice::ScrollUp => Self::ScrollUp,
        }
    }
}

impl From<PollingConnection> for ConnectionType {
    fn from(value: PollingConnection) -> Self {
        match value {
            PollingConnection::Wired => Self::Wired,
            PollingConnection::Wireless => Self::GamingWireless,
        }
    }
}

impl FromStr for DeviceSelector {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if let Some((vendor, product)) = value.split_once(':') {
            if product.contains(':') {
                return Err("expected a list index or VID:PID such as 9 or 046d:c54d".into());
            }
            let vendor_id = u16::from_str_radix(vendor, 16)
                .map_err(|_| format!("invalid hexadecimal vendor ID `{vendor}`"))?;
            let product_id = u16::from_str_radix(product, 16)
                .map_err(|_| format!("invalid hexadecimal product ID `{product}`"))?;
            Ok(Self::UsbId {
                vendor_id,
                product_id,
            })
        } else {
            value
                .parse::<usize>()
                .map(Self::ListIndex)
                .map_err(|_| "expected a list index or VID:PID such as 9 or 046d:c54d".into())
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let api = HidApi::new()?;

    match cli.command {
        Command::List { all } => list_devices(&api, all),
        Command::Devices => managed_devices()?,
        Command::Probe { device } => probe_device(&api, device)?,
        Command::Features { device } => dump_features(&api, device)?,
        Command::Profiles { device } => dump_profiles(&api, device)?,
        Command::ProveProfileWrite {
            device,
            profile,
            confirm_flash_write,
        } => prove_profile_write(&api, device, profile, confirm_flash_write)?,
        Command::ProveProfileNameEdit {
            device,
            target,
            temporary_name,
        } => prove_profile_name_edit(&api, device, target, &temporary_name)?,
        Command::SetProfileName {
            device,
            target,
            name,
            clear_name,
        } => set_profile_name(&api, device, target, name, clear_name)?,
        Command::SetProfileDpi {
            device,
            target,
            stage,
            dpi,
            dpi_y,
            lod,
            make_default,
            make_shift,
        } => set_profile_dpi(
            &api,
            device,
            target,
            stage,
            dpi,
            dpi_y,
            lod,
            make_default,
            make_shift,
        )?,
        Command::SetProfilePollingRate {
            device,
            target,
            hz,
            connection,
        } => set_profile_polling_rate(&api, device, target, hz, connection)?,
        Command::SetProfilePowerTimeouts {
            device,
            target,
            save_seconds,
            off_seconds,
        } => set_profile_power_timeouts(&api, device, target, save_seconds, off_seconds)?,
        Command::SetProfileBhop {
            device,
            target,
            state,
            window_ms,
        } => set_profile_bhop(&api, device, target, state, window_ms)?,
        Command::SetProfileButton {
            device,
            target,
            button,
            action,
        } => set_profile_button(&api, device, target, button, action)?,
        Command::SetProfileEnabled {
            device,
            target,
            state,
        } => set_profile_enabled(&api, device, target, state)?,
        Command::SetDpi { device, dpi } => set_dpi(&api, device, dpi)?,
        Command::SetDpiStage { device, stage } => set_dpi_stage(&api, device, stage)?,
        Command::SetLiftOffDistance { device, lod } => set_lift_off_distance(&api, device, lod)?,
        Command::SetSurfaceMode { device, mode } => set_surface_mode(&api, device, mode)?,
        Command::SetOperatingMode { device, mode } => set_operating_mode(&api, device, mode)?,
        Command::SetLighting {
            device,
            zone,
            effect,
        } => set_lighting(&api, device, zone, effect)?,
        Command::UseFirmwareLighting { device } => use_firmware_lighting(&api, device)?,
        Command::SetBunnyHopping {
            device,
            state,
            window_ms,
        } => set_bunny_hopping(&api, device, state, window_ms)?,
        Command::SetPollingRate {
            device,
            hz,
            connection,
            disable_onboard_profiles,
        } => set_polling_rate(&api, device, hz, connection, disable_onboard_profiles)?,
        Command::UseHostSettings { device } => use_host_settings(&api, device)?,
        Command::UseOnboardProfile { device, profile } => {
            use_onboard_profile(&api, device, profile)?
        }
    }

    Ok(())
}

fn dump_features(api: &HidApi, device: DeviceArgs) -> Result<()> {
    println!("Read-only feature dump; no settings will be changed.");
    let session = open_session(api, device)?;
    for entry in session.feature_set()? {
        let descriptor = describe_feature(entry.feature_id, entry.flags);
        let flag_names = feature_flag_names(entry.flags);
        let flags = if flag_names.is_empty() {
            "public".to_owned()
        } else {
            flag_names.join(", ")
        };
        println!(
            "{:>2}: 0x{:04x} v{} flags 0x{:02x} ({flags}) - {} [{}; {}]",
            entry.index,
            entry.feature_id,
            entry.version,
            entry.flags,
            descriptor.name,
            descriptor.audience,
            descriptor.support,
        );
    }
    Ok(())
}

fn dump_profiles(api: &HidApi, device: DeviceArgs) -> Result<()> {
    println!("Read-only onboard-profile dump; flash and active settings will not be changed.");
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    if matches!(
        mouse.configuration_source()?,
        Some(ConfigurationSource::Onboard { .. })
    ) {
        if let Some(stage) = mouse.current_onboard_dpi_stage()? {
            println!("Active onboard DPI stage: {stage}");
        }
    }
    let profiles = mouse.stored_profiles()?;
    if profiles.is_empty() {
        println!("No stored profiles were found.");
        return Ok(());
    }

    for (index, profile) in profiles.iter().enumerate() {
        println!();
        println!(
            "Profile {}: {}sector 0x{:04x}, CRC {}, name {}",
            index + 1,
            if profile.enabled {
                "enabled, "
            } else {
                "disabled, "
            },
            profile.sector,
            if profile.crc_valid {
                "valid"
            } else {
                "INVALID"
            },
            profile.name.as_deref().unwrap_or("<default>"),
        );
        println!(
            "  Polling: wired {}, wireless {}",
            optional_hz(profile.wired_hz),
            optional_hz(profile.wireless_hz),
        );
        println!(
            "  DPI indices: default {}, shift {}; stages {}",
            profile.default_dpi_index,
            profile.shifted_dpi_index,
            profile
                .dpi_stages
                .iter()
                .enumerate()
                .map(|(stage, dpi)| match (dpi.y, dpi.lod) {
                    (Some(y), Some(lod)) => format!("{stage}:{}x{y}/{lod}", dpi.x),
                    (Some(y), None) => format!("{stage}:{}x{y}", dpi.x),
                    (None, _) => format!("{stage}:{}", dpi.x),
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        if let Some(timeout) = profile.bunny_hopping_timeout_ms {
            println!("  BHOP timeout: {timeout} ms");
        }
        println!(
            "  Power timers: save {}, off {}",
            profile.power_save_timeout, profile.power_off_timeout
        );
        for (button, binding) in profile.buttons.iter().enumerate() {
            println!("  Button {}: {}", button + 1, binding.description());
        }
    }
    Ok(())
}

fn prove_profile_write(
    api: &HidApi,
    device: DeviceArgs,
    profile: u8,
    confirm_flash_write: bool,
) -> Result<()> {
    if !confirm_flash_write {
        bail!(
            "this proof writes onboard flash; retry with `--confirm-flash-write` after selecting a disabled profile"
        );
    }
    println!(
        "Controlled flash proof: profile {profile} must be disabled; identical bytes will be written and verified."
    );
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let proof = mouse.prove_disabled_profile_write(usize::from(profile))?;
    println!(
        "Onboard profile write verified: profile {}, sector 0x{:04x}, {} bytes identical, CRC 0x{:04x} valid.",
        proof.profile_number, proof.sector, proof.bytes_verified, proof.crc
    );
    Ok(())
}

fn prove_profile_name_edit(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    temporary_name: &str,
) -> Result<()> {
    confirm_profile_write(target)?;
    println!(
        "Controlled reversible flash proof: profile {} must be disabled; its exact original sector will be restored.",
        target.profile
    );
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let proof =
        mouse.prove_reversible_profile_name_edit(usize::from(target.profile), temporary_name)?;
    println!(
        "Profile name edit verified and restored: profile {}, sector 0x{:04x}, {} -> {}, {} original bytes restored, CRC 0x{:04x} valid.",
        proof.profile_number,
        proof.sector,
        optional_text(proof.original_name.as_deref()),
        proof.temporary_name,
        proof.bytes_restored,
        proof.original_crc
    );
    Ok(())
}

fn set_profile_name(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    name: Option<String>,
    clear_name: bool,
) -> Result<()> {
    confirm_profile_write(target)?;
    let requested = if clear_name { None } else { name };
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.edit_stored_profile(
        usize::from(target.profile),
        OnboardProfileEdit::Name(requested),
    )?;
    println!(
        "Profile {} name updated and verified: {} -> {}",
        target.profile,
        optional_text(change.before.name.as_deref()),
        optional_text(change.after.name.as_deref())
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn set_profile_dpi(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    stage: u8,
    dpi: u16,
    dpi_y: Option<u16>,
    lod: Option<LiftOffDistanceChoice>,
    make_default: bool,
    make_shift: bool,
) -> Result<()> {
    confirm_profile_write(target)?;
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.edit_stored_profile(
        usize::from(target.profile),
        OnboardProfileEdit::DpiStage {
            index: stage,
            x: dpi,
            y: dpi_y,
            lod: lod.map(Into::into),
            make_default,
            make_shift,
        },
    )?;
    let before = change
        .before
        .dpi_stages
        .get(usize::from(stage))
        .context("DPI stage disappeared from pre-write profile")?;
    let after = change
        .after
        .dpi_stages
        .get(usize::from(stage))
        .context("DPI stage disappeared after write")?;
    println!(
        "Profile {} DPI stage {} updated and verified: {} -> {}",
        target.profile,
        stage,
        profile_dpi_label(before),
        profile_dpi_label(after)
    );
    if make_default || make_shift {
        println!(
            "DPI indices: default {}, shift {}",
            change.after.default_dpi_index, change.after.shifted_dpi_index
        );
    }
    Ok(())
}

fn set_profile_polling_rate(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    hz: u16,
    connection: PollingConnection,
) -> Result<()> {
    confirm_profile_write(target)?;
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.edit_stored_profile(
        usize::from(target.profile),
        OnboardProfileEdit::PollingRate {
            connection: connection.into(),
            hz,
        },
    )?;
    let select = |profile: &open_hub_core::OnboardProfile| match connection {
        PollingConnection::Wired => profile.wired_hz,
        PollingConnection::Wireless => profile.wireless_hz,
    };
    println!(
        "Profile {} {} polling updated and verified: {} -> {}",
        target.profile,
        match connection {
            PollingConnection::Wired => "wired",
            PollingConnection::Wireless => "wireless",
        },
        optional_hz(select(&change.before)),
        optional_hz(select(&change.after))
    );
    Ok(())
}

fn set_profile_power_timeouts(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    save_seconds: u16,
    off_seconds: u16,
) -> Result<()> {
    confirm_profile_write(target)?;
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.edit_stored_profile(
        usize::from(target.profile),
        OnboardProfileEdit::PowerTimeouts {
            save_seconds,
            off_seconds,
        },
    )?;
    println!(
        "Profile {} power timeouts updated and verified: save {}/off {} -> save {}/off {} seconds",
        target.profile,
        change.before.power_save_timeout,
        change.before.power_off_timeout,
        change.after.power_save_timeout,
        change.after.power_off_timeout
    );
    Ok(())
}

fn set_profile_bhop(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    state: ToggleChoice,
    window_ms: u16,
) -> Result<()> {
    confirm_profile_write(target)?;
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.edit_stored_profile(
        usize::from(target.profile),
        OnboardProfileEdit::BunnyHopping {
            timeout_ms: state.enabled().then_some(window_ms),
        },
    )?;
    println!(
        "Profile {} BHOP updated and verified: {} -> {}",
        target.profile,
        profile_bhop_label(change.before.bunny_hopping_timeout_ms),
        profile_bhop_label(change.after.bunny_hopping_timeout_ms)
    );
    Ok(())
}

fn set_profile_button(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    button: u8,
    action: ProfileButtonCommand,
) -> Result<()> {
    confirm_profile_write(target)?;
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.edit_stored_profile(
        usize::from(target.profile),
        OnboardProfileEdit::Button {
            button,
            action: action.into_action(),
        },
    )?;
    let index = usize::from(button - 1);
    println!(
        "Profile {} button {} updated and verified: {} -> {}",
        target.profile,
        button,
        change.before.buttons[index].description(),
        change.after.buttons[index].description()
    );
    Ok(())
}

fn set_profile_enabled(
    api: &HidApi,
    device: DeviceArgs,
    target: ProfileTargetArgs,
    state: ToggleChoice,
) -> Result<()> {
    confirm_profile_write(target)?;
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.set_stored_profile_enabled(usize::from(target.profile), state.enabled())?;
    println!(
        "Profile {} state updated and verified: {} -> {}",
        target.profile,
        if change.before { "enabled" } else { "disabled" },
        if change.after { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn confirm_profile_write(target: ProfileTargetArgs) -> Result<()> {
    if !target.confirm_flash_write {
        bail!(
            "this command rewrites onboard flash; retry with `--confirm-flash-write` after reviewing the selected profile"
        );
    }
    Ok(())
}

fn optional_text(value: Option<&str>) -> &str {
    value.unwrap_or("<default>")
}

fn profile_dpi_label(stage: &open_hub_core::OnboardDpiStage) -> String {
    match (stage.y, stage.lod) {
        (Some(y), Some(lod)) => format!("{}x{y}/{lod}", stage.x),
        (Some(y), None) => format!("{}x{y}", stage.x),
        (None, _) => stage.x.to_string(),
    }
}

fn profile_bhop_label(timeout_ms: Option<u16>) -> String {
    timeout_ms.map_or_else(|| "off".to_owned(), |timeout| format!("on/{timeout} ms"))
}

fn managed_devices() -> Result<()> {
    println!("Automatic read-only discovery; no settings will be changed.");
    let mut manager = DeviceManager::new()?;
    let devices = manager.refresh()?.to_vec();
    if devices.is_empty() {
        println!("No supported Logitech HID++ mice found.");
        return Ok(());
    }

    for device in devices {
        println!();
        println!("[{}]", device.id);
        println!(
            "  {} {:04x}:{:04x}, {}, HID++ index 0x{:02x}",
            device.product_name.as_deref().unwrap_or("<unknown>"),
            device.vendor_id,
            device.product_id,
            match device.connection {
                DeviceConnection::DirectUsb => "direct USB",
                DeviceConnection::Receiver => "receiver",
            },
            device.device_index
        );
        match manager.open(&device.id) {
            Ok(mouse) => print_mouse_snapshot(&mouse)?,
            Err(error) => println!("  Unable to open: {error:#}"),
        }
    }
    Ok(())
}

fn list_devices(api: &HidApi, all: bool) {
    let devices = sorted_devices(api, all);

    if devices.is_empty() {
        println!("No matching HID interfaces found.");
        return;
    }

    for (index, device) in devices.iter().enumerate() {
        println!(
            "[{index}] {:04x}:{:04x}",
            device.vendor_id(),
            device.product_id()
        );
        println!(
            "    product:   {}",
            device.product_string().unwrap_or("<unknown>")
        );
        println!(
            "    serial:    {}",
            device.serial_number().unwrap_or("<none>")
        );
        println!("    bus:       {:?}", device.bus_type());
        println!("    interface: {}", device.interface_number());
        println!(
            "    usage:     page=0x{:04x}, id=0x{:04x}",
            device.usage_page(),
            device.usage()
        );
        println!("    path:      {}", device.path().to_string_lossy());
    }
}

fn probe_device(api: &HidApi, device: DeviceArgs) -> Result<()> {
    println!("Read-only probe; no settings will be changed.");
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    print_mouse_snapshot(&mouse)
}

fn print_mouse_snapshot(mouse: &MouseDevice) -> Result<()> {
    let capabilities = mouse.capabilities()?;
    let settings = mouse.settings()?;

    if let Some(metadata) = mouse.metadata()? {
        println!("Device: {} ({})", metadata.name, metadata.device_type);
        println!(
            "Unit ID: {}; models {:04x}/{:04x}/{:04x}; transport flags 0x{:02x}",
            metadata
                .information
                .unit_id
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>(),
            metadata.information.model_ids[0],
            metadata.information.model_ids[1],
            metadata.information.model_ids[2],
            metadata.information.transport_flags,
        );
        if let Some(serial) = metadata.serial_number {
            println!("Device serial: {serial}");
        }
        for firmware in metadata.firmware {
            println!(
                "Firmware entity {}: {} {:02}.{:02}.B{:04}{}{}",
                firmware.entity_type,
                firmware.prefix,
                firmware.firmware_number,
                firmware.revision,
                firmware.build,
                if firmware.active { " (active)" } else { "" },
                if firmware.transport_pid == 0 {
                    String::new()
                } else {
                    format!(" PID {:04x}", firmware.transport_pid)
                },
            );
        }
    }

    if let Some(battery) = settings.battery {
        println!(
            "Battery: {}% ({}, level code 0x{:02x})",
            battery.percentage,
            battery.status_name(),
            battery.level_code
        );
    } else {
        println!("Battery: unsupported");
    }

    if let Some(dpi) = settings.dpi {
        println!(
            "DPI: X={} (default {}), Y={}, LOD={}",
            dpi.current_x,
            dpi.default_x,
            optional_number(dpi.current_y),
            optional_number(dpi.lod)
        );
        println!(
            "DPI supported: {}",
            capabilities.supported_dpi.as_deref().map_or_else(
                || "<none>".to_owned(),
                |values| summarize_values(values, "")
            )
        );
    } else {
        println!("DPI: unsupported");
    }

    match (&capabilities.polling_rates, settings.polling_rate) {
        (PollingRateCapabilities::Shared { supported_hz }, PollingRateSettings::Shared { hz }) => {
            println!(
                "Polling rate (shared): {hz} Hz; supported {}",
                summarize_values(supported_hz, " Hz")
            )
        }
        (
            PollingRateCapabilities::PerConnection {
                wired_hz,
                wireless_hz,
            },
            PollingRateSettings::PerConnection {
                wired_hz: current_wired,
                wireless_hz: current_wireless,
            },
        ) => {
            println!(
                "Polling rate (wired): {current_wired} Hz; supported {}",
                summarize_values(wired_hz, " Hz")
            );
            println!(
                "Polling rate (wireless): {current_wireless} Hz; supported {}",
                summarize_values(wireless_hz, " Hz")
            );
        }
        (PollingRateCapabilities::Unsupported, PollingRateSettings::Unsupported) => {
            println!("Polling rate: unsupported")
        }
        _ => bail!("inconsistent polling-rate capability and settings representations"),
    }

    if let Some(mode) = settings.mode_status {
        println!(
            "Operating mode: {} (status 0x{:02x}:0x{:02x}, capabilities 0x{:02x}:0x{:02x})",
            if mode.performance() {
                "performance"
            } else {
                "endurance"
            },
            mode.status0,
            mode.status1,
            mode.capabilities,
            mode.capabilities1
        );
        if let Some(surface_mode) = mode.surface_mode {
            println!("Surface mode: {surface_mode}");
        }
    }

    if let Some(bhop) = settings.bunny_hopping {
        println!(
            "BHOP: {}; timeout {}",
            if bhop.enabled { "on" } else { "off" },
            if bhop.timeout_ms == 0 {
                "not configured".to_owned()
            } else {
                format!("{} ms", bhop.timeout_ms)
            }
        );
    }

    if let Some(lighting) = mouse.color_led_state()? {
        println!(
            "Lighting: {} zone(s), NV capabilities 0x{:04x}, extended capabilities 0x{:04x}, control {}",
            lighting.info.zone_count,
            lighting.info.nv_capabilities,
            lighting.info.extended_capabilities,
            lighting
                .software_control
                .map_or("unknown", |software| if software {
                    "software"
                } else {
                    "firmware"
                })
        );
        for zone in lighting.zones {
            let current = zone.current_effect_index.map_or_else(
                || "unknown".to_owned(),
                |index| {
                    zone.effects
                        .iter()
                        .find(|effect| effect.index == index)
                        .map_or_else(
                            || format!("index {index}"),
                            |effect| format!("{} (index {index})", effect.effect_name()),
                        )
                },
            );
            println!(
                "  LED zone {} location 0x{:04x}, persistence 0x{:02x}, current {current}",
                zone.index, zone.location, zone.persistency_capabilities
            );
            println!(
                "    Effects: {}",
                zone.effects
                    .iter()
                    .map(|effect| format!(
                        "{}:{} caps 0x{:04x} period {} ms",
                        effect.index,
                        effect.effect_name(),
                        effect.capabilities,
                        effect.period_ms
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    if let Some(profiles) = capabilities.onboard_profile_description {
        println!(
            "Onboard profile storage: memory 0x{:02x}, format 0x{:02x}, macro 0x{:02x}; \
             {} writable + {} factory profiles, {} buttons, {} sectors of {} bytes",
            profiles.memory_model_id,
            profiles.profile_format_id,
            profiles.macro_format_id,
            profiles.profile_count,
            profiles.factory_profile_count,
            profiles.button_count,
            profiles.sector_count,
            profiles.sector_size,
        );
    }

    if let Some(filter) = mouse.mouse_button_filter()? {
        println!(
            "Host button mapping: {}",
            filter
                .mapping
                .iter()
                .enumerate()
                .map(|(physical, mapped)| format!("{}->{mapped}", physical + 1))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    match settings.configuration_source {
        Some(ConfigurationSource::Host) => {
            println!("Configuration source: host/local settings")
        }
        Some(ConfigurationSource::Onboard { active_profile }) => println!(
            "Configuration source: onboard profile {}",
            active_profile.map_or_else(|| "unknown".to_owned(), |sector| format!("0x{sector:04x}"))
        ),
        Some(ConfigurationSource::Unknown { raw_mode }) => {
            println!("Configuration source: unknown mode 0x{raw_mode:02x}")
        }
        None => println!("Configuration source: profile control unsupported"),
    }
    Ok(())
}

fn open_session(api: &HidApi, args: DeviceArgs) -> Result<HidppSession> {
    let devices = sorted_devices(api, false);
    let (index, info) = select_device(&devices, args.index)?;

    if info.usage_page() < 0xff00 {
        bail!(
            "interface {index} has non-vendor usage page 0x{:04x}; select a Logitech vendor-defined interface",
            info.usage_page()
        );
    }

    let device_index = args
        .device_index
        .unwrap_or_else(|| default_device_index(info));
    println!(
        "Opening [{index}] {:04x}:{:04x}, interface {}, usage 0x{:04x}:0x{:04x}, HID++ device index 0x{device_index:02x}",
        info.vendor_id(),
        info.product_id(),
        info.interface_number(),
        info.usage_page(),
        info.usage(),
    );

    let short_info = find_report_collection(&devices, info, 0x0001).unwrap_or(info);
    let long_info = find_report_collection(&devices, info, 0x0002);

    let short_device = short_info.open_device(api).with_context(|| {
        format!(
            "failed to open {}; close G HUB and retry if it owns this interface",
            short_info.path().to_string_lossy()
        )
    })?;
    let long_device = long_info
        .filter(|long_info| long_info.path() != short_info.path())
        .map(|long_info| {
            long_info.open_device(api).with_context(|| {
                format!(
                    "failed to open companion long-report collection {}",
                    long_info.path().to_string_lossy()
                )
            })
        })
        .transpose()?;
    let session = HidppSession::new(
        short_device,
        long_device,
        device_index,
        Duration::from_millis(args.timeout_ms),
    );

    let protocol = session.protocol_version()?;
    println!("HID++ protocol: {}.{}", protocol.major, protocol.minor);
    if protocol.major < 2 {
        bail!("this first probe supports HID++ 2.0 and newer devices only");
    }

    Ok(session)
}

fn set_dpi(api: &HidApi, device: DeviceArgs, requested_dpi: u16) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.set_dpi(requested_dpi)?;
    println!(
        "DPI updated and verified: X {} -> {}, Y {} -> {}",
        change.before.current_x,
        change.after.current_x,
        optional_number(change.before.current_y),
        optional_number(change.after.current_y)
    );
    Ok(())
}

fn set_dpi_stage(api: &HidApi, device: DeviceArgs, stage: u8) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.set_current_onboard_dpi_stage(stage)?;
    println!(
        "Onboard DPI stage updated and verified: {} -> {}",
        change.before, change.after
    );
    Ok(())
}

fn set_lift_off_distance(
    api: &HidApi,
    device: DeviceArgs,
    lod: LiftOffDistanceChoice,
) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.set_lift_off_distance(lod.into())?;
    println!(
        "Lift-off distance updated and verified: {} -> {}",
        change.before, change.after
    );
    Ok(())
}

fn set_surface_mode(api: &HidApi, device: DeviceArgs, mode: SurfaceModeChoice) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.set_surface_mode(mode.into())?;
    println!(
        "Surface mode updated and verified: {} -> {}",
        change.before, change.after
    );
    Ok(())
}

fn set_operating_mode(api: &HidApi, device: DeviceArgs, mode: OperatingModeChoice) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.set_operating_mode(mode.into())?;
    println!(
        "Operating mode updated and verified: {} -> {}",
        change.before, change.after
    );
    Ok(())
}

fn set_lighting(
    api: &HidApi,
    device: DeviceArgs,
    zone: u8,
    command: LightingCommand,
) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let effect = command.into_effect();
    let settings = mouse.set_color_led_effect(zone, effect)?;
    match settings {
        Some(settings) => println!(
            "Volatile lighting updated and verified for zone {}, effect {}",
            settings.zone_index, settings.zone_effect_index
        ),
        None => println!(
            "Volatile lighting updated for zone {zone}; this firmware does not expose read-back verification"
        ),
    }
    println!(
        "No onboard flash was changed; use `use-firmware-lighting` to release software control."
    );
    Ok(())
}

fn use_firmware_lighting(api: &HidApi, device: DeviceArgs) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    mouse.release_color_led_control()?;
    println!("Color LED control returned to the mouse firmware.");
    Ok(())
}

fn set_bunny_hopping(
    api: &HidApi,
    device: DeviceArgs,
    state: ToggleChoice,
    timeout_ms: u16,
) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    let change = mouse.set_bunny_hopping(state.enabled(), timeout_ms)?;
    println!(
        "BHOP updated and verified: {} ({} ms) -> {} ({} ms)",
        if change.before.enabled { "on" } else { "off" },
        change.before.timeout_ms,
        if change.after.enabled { "on" } else { "off" },
        change.after.timeout_ms
    );
    Ok(())
}

fn set_polling_rate(
    api: &HidApi,
    device: DeviceArgs,
    requested_hz: u16,
    connection: PollingConnection,
    disable_onboard_profiles: bool,
) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;

    if let Some(source) = mouse.configuration_source()? {
        if matches!(source, ConfigurationSource::Onboard { .. }) {
            if !disable_onboard_profiles {
                bail!(
                    "an onboard profile is active and controls the polling rate; retry with \
                     `--disable-onboard-profiles` to switch to host control without erasing profiles"
                );
            }

            mouse.set_host_control()?;
            if mouse.configuration_source()? != Some(ConfigurationSource::Host) {
                bail!("failed to switch from onboard profiles to host control");
            }
            println!("Onboard profiles disabled; the mouse is now host-controlled.");
        }
    }

    let change = mouse.set_polling_rate(connection.into(), requested_hz)?;
    println!(
        "Polling rate ({}) updated and verified: {} Hz -> {} Hz",
        match connection {
            PollingConnection::Wired => "wired",
            PollingConnection::Wireless => "wireless",
        },
        change.before,
        change.after
    );
    Ok(())
}

fn use_host_settings(api: &HidApi, device: DeviceArgs) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    mouse.set_host_control()?;
    if mouse.configuration_source()? != Some(ConfigurationSource::Host) {
        bail!("host/local settings verification failed");
    }
    println!("Configuration source updated and verified: host/local settings");
    Ok(())
}

fn use_onboard_profile(api: &HidApi, device: DeviceArgs, profile: u16) -> Result<()> {
    let mouse = MouseDevice::discover(open_session(api, device)?)?;
    mouse.activate_onboard_profile(profile)?;
    let expected = Some(ConfigurationSource::Onboard {
        active_profile: Some(profile),
    });
    if mouse.configuration_source()? != expected {
        bail!("onboard profile verification failed for sector 0x{profile:04x}");
    }
    println!("Configuration source updated and verified: onboard profile 0x{profile:04x}");
    Ok(())
}

fn sorted_devices(api: &HidApi, all: bool) -> Vec<DeviceInfo> {
    let mut devices: Vec<DeviceInfo> = api
        .device_list()
        .filter(|device| all || device.vendor_id() == LOGITECH_VENDOR_ID)
        .cloned()
        .collect();

    devices.sort_by(|left, right| {
        (
            left.vendor_id(),
            left.product_id(),
            left.interface_number(),
            left.usage_page(),
            left.usage(),
            left.path().to_bytes(),
        )
            .cmp(&(
                right.vendor_id(),
                right.product_id(),
                right.interface_number(),
                right.usage_page(),
                right.usage(),
                right.path().to_bytes(),
            ))
    });
    devices
}

fn select_device(devices: &[DeviceInfo], selector: DeviceSelector) -> Result<(usize, &DeviceInfo)> {
    match selector {
        DeviceSelector::ListIndex(index) => devices
            .get(index)
            .map(|info| (index, info))
            .with_context(|| {
                format!("device index {index} does not exist; run `open-hub-probe list` again")
            }),
        DeviceSelector::UsbId {
            vendor_id,
            product_id,
        } => {
            let mut matches = devices.iter().enumerate().filter(|(_, info)| {
                info.vendor_id() == vendor_id
                    && info.product_id() == product_id
                    && info.usage_page() >= 0xff00
                    && info.usage() == 0x0001
            });
            let selected = matches.next().with_context(|| {
                format!(
                    "no HID++ short-report collection found for {vendor_id:04x}:{product_id:04x}; run `open-hub-probe list`"
                )
            })?;
            if matches.next().is_some() {
                bail!(
                    "more than one HID++ device matches {vendor_id:04x}:{product_id:04x}; use its numeric list index"
                );
            }
            Ok(selected)
        }
    }
}

fn find_report_collection<'a>(
    devices: &'a [DeviceInfo],
    selected: &DeviceInfo,
    usage: u16,
) -> Option<&'a DeviceInfo> {
    devices.iter().find(|candidate| {
        candidate.vendor_id() == selected.vendor_id()
            && candidate.product_id() == selected.product_id()
            && candidate.interface_number() == selected.interface_number()
            && candidate.serial_number() == selected.serial_number()
            && candidate.usage_page() == selected.usage_page()
            && candidate.usage() == usage
    })
}

fn default_device_index(info: &DeviceInfo) -> u8 {
    let receiver = info
        .product_string()
        .is_some_and(|product| product.to_ascii_lowercase().contains("receiver"));
    if receiver { 0x01 } else { 0xff }
}

fn parse_u8(value: &str) -> std::result::Result<u8, String> {
    let value = value.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    u8::from_str_radix(value, 16).map_err(|error| format!("expected a hex byte: {error}"))
}

fn parse_u16(value: &str) -> std::result::Result<u16, String> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16).map_err(|error| format!("expected a profile sector: {error}"))
    } else {
        value
            .parse::<u16>()
            .map_err(|error| format!("expected a profile sector: {error}"))
    }
}

fn parse_rgb(value: &str) -> std::result::Result<RgbColor, String> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected an RGB color as RRGGBB or #RRGGBB".to_owned());
    }
    Ok(RgbColor {
        red: u8::from_str_radix(&hex[0..2], 16).expect("validated hexadecimal pair"),
        green: u8::from_str_radix(&hex[2..4], 16).expect("validated hexadecimal pair"),
        blue: u8::from_str_radix(&hex[4..6], 16).expect("validated hexadecimal pair"),
    })
}

fn optional_number<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| value.to_string())
}

fn optional_hz(value: Option<u16>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| format!("{value} Hz"))
}

fn summarize_values(values: &[u16], suffix: &str) -> String {
    if values.is_empty() {
        return "<none>".to_owned();
    }
    if values.len() <= 10 {
        return values
            .iter()
            .map(|value| format!("{value}{suffix}"))
            .collect::<Vec<_>>()
            .join(", ");
    }

    format!(
        "{}{suffix}..{}{suffix} ({} values)",
        values[0],
        values[values.len() - 1],
        values.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_device_selector() {
        assert!(matches!(
            "9".parse::<DeviceSelector>().unwrap(),
            DeviceSelector::ListIndex(9)
        ));
    }

    #[test]
    fn parses_usb_device_selector() {
        assert!(matches!(
            "046d:c54d".parse::<DeviceSelector>().unwrap(),
            DeviceSelector::UsbId {
                vendor_id: 0x046d,
                product_id: 0xc54d
            }
        ));
    }

    #[test]
    fn parses_decimal_and_hex_profile_sectors() {
        assert_eq!(parse_u16("1").unwrap(), 1);
        assert_eq!(parse_u16("0x0101").unwrap(), 0x0101);
    }

    #[test]
    fn parses_rgb_colors() {
        assert_eq!(
            parse_rgb("#12aBcD").unwrap(),
            RgbColor {
                red: 0x12,
                green: 0xab,
                blue: 0xcd
            }
        );
        assert!(parse_rgb("12345").is_err());
        assert!(parse_rgb("gg0000").is_err());
    }

    #[test]
    fn clap_command_definition_is_valid() {
        use clap::CommandFactory;

        Cli::command().debug_assert();
    }
}
