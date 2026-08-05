use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use open_hub_protocol::RgbColor;

#[derive(Debug, Parser)]
#[command(name = "open-hub", version, propagate_version = true)]
pub struct Cli {
    #[arg(long, value_enum, default_value_t = OutputFormat::Human, global = true)]
    pub format: OutputFormat,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Status,
    Device {
        #[command(subcommand)]
        command: DeviceCommand,
    },
    Events {
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        count: Option<u64>,
    },
    Completions {
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    List,
    Show { selector: Option<String> },
    Set(DeviceSetArgs),
    Use(DeviceUseArgs),
}

#[derive(Debug, Args)]
pub struct DeviceSetArgs {
    pub selector: Option<String>,
    #[command(subcommand)]
    pub setting: SettingCommand,
}

#[derive(Debug, Subcommand)]
pub enum SettingCommand {
    Dpi {
        dpi: u16,
    },
    #[command(name = "polling-rate")]
    PollingRate {
        hz: u16,
        #[arg(long, value_enum, default_value_t = ConnectionChoice::Wireless)]
        connection: ConnectionChoice,
        #[arg(long)]
        disable_onboard_profiles: bool,
    },
    #[command(name = "lift-off-distance")]
    LiftOffDistance {
        lod: LiftOffDistanceChoice,
    },
    #[command(name = "surface-mode")]
    SurfaceMode {
        mode: SurfaceModeChoice,
    },
    #[command(name = "operating-mode")]
    OperatingMode {
        mode: OperatingModeChoice,
    },
    Bhop {
        state: Toggle,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(100..=1000))]
        window_ms: u16,
    },
    Lighting {
        #[command(subcommand)]
        effect: LightingCommand,
    },
}

#[derive(Debug, Args)]
pub struct DeviceUseArgs {
    pub selector: Option<String>,
    #[command(subcommand)]
    pub mode: UseCommand,
}

#[derive(Debug, Subcommand)]
pub enum UseCommand {
    #[command(name = "firmware-lighting")]
    FirmwareLighting,
    #[command(name = "host-settings")]
    HostSettings,
    #[command(name = "onboard-profile")]
    OnboardProfile {
        #[arg(value_parser = parse_sector)]
        sector: u16,
    },
}

#[derive(Debug, Subcommand)]
pub enum LightingCommand {
    Off,
    Fixed {
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
        #[arg(long, value_parser = parse_rgb)]
        color: RgbColor,
        #[arg(long, default_value_t = 1000)]
        period_ms: u16,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(1..=100))]
        brightness: u8,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    pub const fn enabled(self) -> bool {
        matches!(self, Self::On)
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConnectionChoice {
    Wired,
    Wireless,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LiftOffDistanceChoice {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SurfaceModeChoice {
    On,
    Automatic,
    Off,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OperatingModeChoice {
    Performance,
    Endurance,
}

pub fn parse_rgb(value: &str) -> Result<RgbColor, String> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        return Err("color must be exactly six hexadecimal digits (RRGGBB)".to_owned());
    }
    let red = u8::from_str_radix(&value[0..2], 16).map_err(|_| "invalid red component")?;
    let green = u8::from_str_radix(&value[2..4], 16).map_err(|_| "invalid green component")?;
    let blue = u8::from_str_radix(&value[4..6], 16).map_err(|_| "invalid blue component")?;
    Ok(RgbColor { red, green, blue })
}

pub fn parse_sector(value: &str) -> Result<u16, String> {
    let (radix, digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or((10, value), |digits| (16, digits));
    u16::from_str_radix(digits, radix)
        .map_err(|_| "sector must be a u16 decimal or hexadecimal value".to_owned())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_representative_commands() {
        assert!(Cli::try_parse_from(["open-hub", "status"]).is_ok());
        assert!(Cli::try_parse_from(["open-hub", "device", "list"]).is_ok());
        assert!(Cli::try_parse_from(["open-hub", "device", "show", "mouse"]).is_ok());
        assert!(Cli::try_parse_from(["open-hub", "device", "set", "dpi", "1600"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "open-hub",
                "device",
                "set",
                "mouse",
                "polling-rate",
                "1000",
                "--connection",
                "wired"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["open-hub", "device", "set", "surface-mode", "automatic"]).is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "open-hub",
                "device",
                "set",
                "bhop",
                "on",
                "--window-ms",
                "100"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "open-hub", "device", "set", "lighting", "fixed", "--color", "a1B2c3"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["open-hub", "device", "use", "onboard-profile", "3"]).is_ok());
        assert!(Cli::try_parse_from(["open-hub", "events", "--count", "1"]).is_ok());
        assert!(Cli::try_parse_from(["open-hub", "events", "--count", "0"]).is_err());
        assert!(Cli::try_parse_from(["open-hub", "completions", "powershell"]).is_ok());
    }

    #[test]
    fn parses_rgb() {
        assert_eq!(
            parse_rgb("#0102fF").unwrap(),
            RgbColor {
                red: 1,
                green: 2,
                blue: 255
            }
        );
        assert!(parse_rgb("123").is_err());
    }

    #[test]
    fn parses_decimal_and_hex_sectors() {
        assert_eq!(parse_sector("17").unwrap(), 17);
        assert_eq!(parse_sector("0x10").unwrap(), 16);
        assert!(parse_sector("0x10000").is_err());
    }
}
