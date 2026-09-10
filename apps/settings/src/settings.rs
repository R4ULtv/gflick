//! Capability-driven form definitions and validated, ordered write plans.
use anyhow::{Result, bail, ensure};
use gflick_protocol::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Dpi,
    Wired,
    Wireless,
    Lod,
    Surface,
    Operating,
    Bhop,
    Timeout,
    Source,
    Nickname,
    Appearance,
    Lighting,
    Color,
    Period,
    Brightness,
    Zone,
}
pub type Values = BTreeMap<Key, String>;

pub struct Spec {
    pub key: Key,
    pub group: &'static str,
    pub label: &'static str,
    pub help: String,
    /// Shown inside the input rather than appended to the label, so the label
    /// stays a name and the unit stays next to the number it qualifies.
    pub unit: &'static str,
    pub value: String,
    pub choices: Vec<(String, String)>,
    pub text: bool,
}
fn spec(
    key: Key,
    group: &'static str,
    label: &'static str,
    value: impl ToString,
    choices: &[(&str, &str)],
) -> Spec {
    Spec {
        key,
        group,
        label,
        help: String::new(),
        unit: "",
        value: value.to_string(),
        choices: choices
            .iter()
            .map(|(v, l)| (v.to_string(), l.to_string()))
            .collect(),
        text: choices.is_empty(),
    }
}
fn rates(key: Key, label: &'static str, value: Option<u16>, rates: &[u16]) -> Spec {
    let mut s = spec(
        key,
        "Polling rate",
        label,
        value.map(|v| v.to_string()).unwrap_or_default(),
        &[],
    );
    s.choices = rates
        .iter()
        .map(|v| (v.to_string(), format!("{v} Hz")))
        .collect();
    s.text = false;
    s
}
pub fn specs(state: &DeviceState) -> Vec<Spec> {
    use Key::*;
    let caps = &state.capabilities;
    let settings = &state.settings;
    let mut fields = Vec::new();
    if let Some(values) = caps.supported_dpi.as_ref().filter(|v| !v.is_empty()) {
        let mut s = spec(
            Dpi,
            "Sensitivity",
            "DPI",
            settings
                .dpi
                .map(|d| d.current_x.to_string())
                .unwrap_or_default(),
            &[],
        );
        s.unit = "DPI";
        s.help = format!(
            "Applies to both axes. Supported range {}–{}.",
            values.iter().min().unwrap(),
            values.iter().max().unwrap()
        );
        s.choices = [400, 800, 1200, 1600, 3200]
            .into_iter()
            .filter(|v| values.contains(v))
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        fields.push(s);
    }
    match &caps.polling_rates {
        PollingRateCapabilities::Shared { supported_hz } => fields.push(rates(
            Wired,
            "Report rate",
            match settings.polling_rate {
                PollingRateState::Shared { hz } => Some(hz),
                _ => None,
            },
            supported_hz,
        )),
        PollingRateCapabilities::PerConnection {
            wired_hz,
            wireless_hz,
        } => {
            let (wired, wireless) = match settings.polling_rate {
                PollingRateState::PerConnection {
                    wired_hz,
                    wireless_hz,
                } => (Some(wired_hz), Some(wireless_hz)),
                _ => (None, None),
            };
            fields.push(rates(Wired, "Wired", wired, wired_hz));
            fields.push(rates(Wireless, "Wireless", wireless, wireless_hz));
        }
        _ => {}
    }
    if caps.lift_off_distance {
        let mut lod = spec(
            Lod,
            "Sensor",
            "Lift-off distance",
            match settings.dpi.and_then(|d| d.lift_off_distance) {
                Some(LiftOffDistance::Low) => "low",
                Some(LiftOffDistance::Medium) => "medium",
                Some(LiftOffDistance::High) => "high",
                None => "",
            },
            &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
        );
        lod.help = "How far the mouse can leave the pad before it stops tracking. Low cuts out \
                    soonest."
            .into();
        fields.push(lod);
    }
    if caps.surface_mode {
        let mut surface = spec(
            Surface,
            "Sensor",
            "Surface mode",
            match settings.surface_mode {
                Some(SurfaceMode::On) => "on",
                Some(SurfaceMode::Automatic) => "automatic",
                Some(SurfaceMode::Off) => "off",
                None => "",
            },
            &[("on", "On"), ("automatic", "Automatic"), ("off", "Off")],
        );
        surface.help = "Whether the sensor tunes itself to the surface under it. Automatic leaves \
                        that to the mouse."
            .into();
        fields.push(surface);
    }
    if caps.operating_mode_switch {
        let mut operating = spec(
            Operating,
            "Sensor",
            "Operating mode",
            match settings.operating_mode {
                Some(OperatingMode::Endurance) => "endurance",
                Some(OperatingMode::Performance) => "performance",
                None => "",
            },
            &[("endurance", "Endurance"), ("performance", "Performance")],
        );
        operating.help = "Performance runs the sensor at full speed. Endurance trades some of \
                          that for battery life."
            .into();
        fields.push(operating);
    }
    if caps.bunny_hopping {
        let mut enabled = spec(
            Bhop,
            "Bunny hopping",
            "Bunny hopping",
            settings
                .bunny_hopping
                .as_ref()
                .map(|b| if b.enabled { "on" } else { "off" })
                .unwrap_or(""),
            &[("off", "Off"), ("on", "On")],
        );
        // Disabling this writes a zero timeout rather than a separate flag.
        enabled.help =
            "Runs for the timeout below. Switching it off clears that timeout on the device."
                .into();
        fields.push(enabled);
        let mut timeout = spec(
            Timeout,
            "Bunny hopping",
            "Timeout",
            settings
                .bunny_hopping
                .as_ref()
                .map(|b| b.timeout_ms.to_string())
                .unwrap_or_default(),
            &[],
        );
        timeout.unit = "ms";
        timeout.help =
            "100–1000 ms, in 100 ms steps. Choose a timeout before enabling bunny hopping.".into();
        fields.push(timeout);
    }
    if caps.onboard_profiles {
        let mut choices = vec![("host".to_owned(), "Host control".to_owned())];
        if let Some(description) = caps.onboard_profile_description {
            for p in 1..=description.profile_count {
                choices.push((p.to_string(), format!("Profile {p}")));
            }
        }
        let value = match settings.configuration_source {
            Some(ConfigurationSource::Host) => "host".to_owned(),
            Some(ConfigurationSource::Onboard {
                active_profile: Some(p),
            }) => p.to_string(),
            _ => String::new(),
        };
        if !value.is_empty() && !choices.iter().any(|(v, _)| v == &value) {
            choices.push((value.clone(), format!("Profile {value}")));
        }
        let mut s = spec(Source, "Configuration", "Settings source", value, &[]);
        s.choices = choices;
        s.text = false;
        s.help="Polling changes require Host control. Select it explicitly before applying. Profile selection activates an existing profile; it does not rewrite it.".into();
        fields.push(s);
    }
    if caps.color_led_effects {
        let mut s = spec(
            Lighting,
            "Lighting",
            "Effect",
            "unchanged",
            &[
                ("unchanged", "Unchanged"),
                ("firmware", "Firmware"),
                ("off", "Off"),
                ("fixed", "Fixed"),
                ("cycling", "Cycling"),
                ("breathing", "Breathing"),
            ],
        );
        s.help =
            "Choose an effect to apply. Current lighting values are not reported by the agent."
                .into();
        fields.push(s);
        let mut color = spec(Color, "Lighting", "Color", "FFFFFF", &[]);
        color.unit = "hex";
        color.help = "Six hexadecimal digits, as RRGGBB.".into();
        fields.push(color);
        let mut period = spec(Period, "Lighting", "Period", "2000", &[]);
        period.unit = "ms";
        period.help = "One full cycle of the breathing or cycling effect.".into();
        fields.push(period);
        let mut brightness = spec(Brightness, "Lighting", "Brightness", "100", &[]);
        brightness.unit = "%";
        fields.push(brightness);
        let mut zone = spec(Zone, "Lighting", "Zone", "0", &[]);
        zone.help = "The lighting zone to write. Most mice expose a single zone.".into();
        fields.push(zone);
    }
    let mut nickname = spec(
        Nickname,
        "Presentation",
        "Nickname",
        state.device.nickname.as_deref().unwrap_or(""),
        &[],
    );
    nickname.help = "Leave empty to use the reported device name.".into();
    fields.push(nickname);
    if state.device.supports_color_selection() {
        let mut color = spec(
            Appearance,
            "Presentation",
            "Mouse color",
            state.device.color.as_str(),
            &state
                .device
                .available_colors()
                .iter()
                .map(|color| {
                    let label = match color {
                        DeviceColor::White => "White",
                        DeviceColor::Black => "Black",
                        DeviceColor::Red => "Red",
                        DeviceColor::Magenta => "Magenta",
                        DeviceColor::Cyan => "Cyan",
                        DeviceColor::Blue => "Blue",
                        DeviceColor::Lilac => "Lilac",
                        DeviceColor::Mint => "Mint",
                    };
                    (color.as_str(), label)
                })
                .collect::<Vec<_>>(),
        );
        color.help =
            "Choose your mouse’s finish. Saved for this mouse when you select Apply.".into();
        fields.push(color);
    }
    fields
}

#[derive(Debug)]
pub struct Change {
    pub keys: Vec<Key>,
    pub command: RequestCommand,
}

pub fn plan(state: &DeviceState, values: &Values, dirty: &Values) -> Result<Vec<Change>> {
    use Key::*;
    ensure!(state.device.ready, "The device is not ready.");
    let available = specs(state);
    for key in dirty.keys() {
        ensure!(
            available.iter().any(|s| s.key == *key),
            "This device does not support {key:?}."
        );
    }
    let get = |key| values.get(&key).map(String::as_str).unwrap_or("").trim();
    let number = |key, label: &str| -> Result<u16> {
        get(key)
            .parse()
            .map_err(|_| anyhow::anyhow!("Enter a whole number from 0 to 65535 for {label}."))
    };
    let mut changes = Vec::new();
    let id = state.device.id.clone();
    if dirty.contains_key(&Source) {
        let command = if get(Source) == "host" {
            RequestCommand::UseHostSettings {
                device_id: id.clone(),
            }
        } else {
            let profile = number(Source, "profile")?;
            ensure!(
                profile > 0
                    && available
                        .iter()
                        .find(|s| s.key == Source)
                        .is_some_and(|s| s.choices.iter().any(|(v, _)| v == get(Source))),
                "Choose a reported onboard profile."
            );
            RequestCommand::UseOnboardProfile {
                device_id: id.clone(),
                profile,
            }
        };
        // Switching profiles can replace every current setting. Require a separate
        // apply, so subsequent edits start from that profile's verified values.
        ensure!(
            get(Source) == "host"
                || dirty
                    .keys()
                    .all(|k| matches!(k, Source | Nickname | Appearance)),
            "Apply the onboard profile first, then edit its settings."
        );
        changes.push(Change {
            keys: vec![Source],
            command,
        });
    }
    if dirty.contains_key(&Dpi) {
        let dpi = number(Dpi, "DPI")?;
        ensure!(
            state
                .capabilities
                .supported_dpi
                .as_ref()
                .is_some_and(|v| v.contains(&dpi)),
            "That DPI is not supported by this mouse. Choose a reported increment."
        );
        changes.push(Change {
            keys: vec![Dpi],
            command: RequestCommand::SetDpi {
                device_id: id.clone(),
                dpi,
            },
        });
    }
    for (key, connection) in [(Wired, Connection::Wired), (Wireless, Connection::Wireless)] {
        if !dirty.contains_key(&key) {
            continue;
        }
        let hz = number(key, "polling rate")?;
        let supported = match &state.capabilities.polling_rates {
            PollingRateCapabilities::Shared { supported_hz } if key == Wired => {
                supported_hz.contains(&hz)
            }
            PollingRateCapabilities::PerConnection {
                wired_hz,
                wireless_hz,
            } => {
                if key == Wired {
                    wired_hz.contains(&hz)
                } else {
                    wireless_hz.contains(&hz)
                }
            }
            _ => false,
        };
        ensure!(supported, "That polling rate is not supported.");
        ensure!(
            !matches!(
                state.settings.configuration_source,
                Some(ConfigurationSource::Onboard { .. })
            ) || (dirty.contains_key(&Source) && get(Source) == "host"),
            "Select Host control under Configuration to change polling rate."
        );
        changes.push(Change {
            keys: vec![key],
            command: RequestCommand::SetPollingRate {
                device_id: id.clone(),
                connection,
                hz,
                disable_onboard_profiles: false,
            },
        });
    }
    for key in [Lod, Surface, Operating] {
        if !dirty.contains_key(&key) {
            continue;
        }
        let command = match key {
            Lod => RequestCommand::SetLiftOffDistance {
                device_id: id.clone(),
                lod: match get(key) {
                    "low" => LiftOffDistance::Low,
                    "medium" => LiftOffDistance::Medium,
                    "high" => LiftOffDistance::High,
                    _ => bail!("Choose a lift-off distance."),
                },
            },
            Surface => RequestCommand::SetSurfaceMode {
                device_id: id.clone(),
                mode: match get(key) {
                    "on" => SurfaceMode::On,
                    "automatic" => SurfaceMode::Automatic,
                    "off" => SurfaceMode::Off,
                    _ => bail!("Choose a surface mode."),
                },
            },
            _ => RequestCommand::SetOperatingMode {
                device_id: id.clone(),
                mode: match get(key) {
                    "endurance" => OperatingMode::Endurance,
                    "performance" => OperatingMode::Performance,
                    _ => bail!("Choose an operating mode."),
                },
            },
        };
        changes.push(Change {
            keys: vec![key],
            command,
        });
    }
    if dirty.contains_key(&Bhop) || dirty.contains_key(&Timeout) {
        ensure!(
            matches!(get(Bhop), "on" | "off"),
            "Choose a bunny hopping mode."
        );
        let timeout_ms = number(Timeout, "timeout")?;
        ensure!(
            (100..=1000).contains(&timeout_ms) && timeout_ms % 100 == 0,
            "Bunny hopping timeout must be 100–1000 ms in 100 ms steps."
        );
        changes.push(Change {
            keys: vec![Bhop, Timeout],
            command: RequestCommand::SetBunnyHopping {
                device_id: id.clone(),
                enabled: get(Bhop) == "on",
                timeout_ms,
            },
        });
    }
    let lighting_keys = vec![Lighting, Color, Period, Brightness, Zone];
    if lighting_keys.iter().any(|k| dirty.contains_key(k)) {
        let command = if get(Lighting) == "firmware" {
            RequestCommand::UseFirmwareLighting {
                device_id: id.clone(),
            }
        } else {
            let color = || -> Result<RgbColor> {
                let hex = get(Color).trim_start_matches('#');
                ensure!(hex.len() == 6, "Enter a six-digit RGB color.");
                let value = u32::from_str_radix(hex, 16)
                    .map_err(|_| anyhow::anyhow!("Enter a valid RGB color."))?;
                Ok(RgbColor {
                    red: (value >> 16) as u8,
                    green: (value >> 8) as u8,
                    blue: value as u8,
                })
            };
            let timing = || -> Result<(u16, u8)> {
                let period = number(Period, "period")?;
                let brightness = number(Brightness, "brightness")?;
                ensure!(
                    period > 0 && (1..=100).contains(&brightness),
                    "Period must be positive and brightness must be 1–100%."
                );
                Ok((period, brightness as u8))
            };
            let effect = match get(Lighting) {
                "off" => LightingEffect::Disabled,
                "fixed" => LightingEffect::Fixed { color: color()? },
                "cycling" => {
                    let (period_ms, brightness) = timing()?;
                    LightingEffect::Cycling {
                        period_ms,
                        brightness,
                    }
                }
                "breathing" => {
                    let (period_ms, brightness) = timing()?;
                    LightingEffect::Breathing {
                        color: color()?,
                        period_ms,
                        brightness,
                    }
                }
                _ => bail!("Choose a lighting effect to apply."),
            };
            let zone = number(Zone, "zone")?;
            ensure!(zone <= 255, "Zone must be 0–255.");
            RequestCommand::SetLighting {
                device_id: id.clone(),
                zone: zone as u8,
                effect,
            }
        };
        changes.push(Change {
            keys: lighting_keys,
            command,
        });
    }
    if dirty.contains_key(&Appearance) {
        let color = state
            .device
            .available_colors()
            .iter()
            .copied()
            .find(|color| color.as_str() == get(Appearance))
            .ok_or_else(|| anyhow::anyhow!("Choose an available mouse color."))?;
        changes.push(Change {
            keys: vec![Appearance],
            command: RequestCommand::SetDeviceColor {
                device_id: id.clone(),
                color,
            },
        });
    }
    if dirty.contains_key(&Nickname) {
        let nickname = get(Nickname);
        ensure!(
            nickname.chars().count() <= 64,
            "Device names must be at most 64 characters."
        );
        changes.push(Change {
            keys: vec![Nickname],
            command: RequestCommand::SetDeviceNickname {
                device_id: id,
                nickname: if nickname.is_empty() {
                    None
                } else {
                    Some(nickname.into())
                },
            },
        });
    }
    Ok(changes)
}

/// Rebase drafts after a batch: confirmed values win for successful commands;
/// failed or unattempted edits survive, including when an earlier write succeeds.
pub fn reconcile(
    state: &DeviceState,
    submitted: &Values,
    pending: &Values,
    applied: &[Key],
) -> (Values, Values) {
    let mut initial: Values = specs(state).into_iter().map(|s| (s.key, s.value)).collect();
    for key in [
        Key::Lighting,
        Key::Color,
        Key::Period,
        Key::Brightness,
        Key::Zone,
    ] {
        if applied.contains(&key)
            && initial.contains_key(&key)
            && let Some(value) = submitted.get(&key)
        {
            initial.insert(key, value.clone());
        }
    }
    let mut draft = initial.clone();
    for (key, value) in pending {
        if !applied.contains(key) && draft.contains_key(key) {
            draft.insert(*key, value.clone());
        }
    }
    (initial, draft)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
