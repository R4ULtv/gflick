use super::*;
use crate::test_support::device;
fn draft(state: &DeviceState, updates: &[(Key, &str)]) -> (Values, Values) {
    let mut values: Values = specs(state).into_iter().map(|s| (s.key, s.value)).collect();
    let dirty: Values = updates
        .iter()
        .map(|(key, value)| (*key, value.to_string()))
        .collect();
    values.extend(dirty.clone());
    (values, dirty)
}
#[test]
fn validates_dpi_against_discrete_capabilities() {
    let state = device();
    for invalid in ["801", "0", "65536", "abc", ""] {
        let (v, d) = draft(&state, &[(Key::Dpi, invalid)]);
        assert!(plan(&state, &v, &d).is_err());
    }
    let (v, d) = draft(&state, &[(Key::Dpi, "1600")]);
    assert!(matches!(
        plan(&state, &v, &d).unwrap()[0].command,
        RequestCommand::SetDpi { dpi: 1600, .. }
    ));
}
#[test]
fn polling_requires_explicit_host_control_and_valid_transport_rate() {
    let state = device();
    let (v, d) = draft(&state, &[(Key::Wireless, "2000")]);
    assert!(plan(&state, &v, &d).is_err());
    let (v, d) = draft(&state, &[(Key::Source, "host"), (Key::Wireless, "2000")]);
    let changes = plan(&state, &v, &d).unwrap();
    assert!(matches!(
        changes[0].command,
        RequestCommand::UseHostSettings { .. }
    ));
    assert!(matches!(
        changes[1].command,
        RequestCommand::SetPollingRate {
            connection: Connection::Wireless,
            hz: 2000,
            disable_onboard_profiles: false,
            ..
        }
    ));
    let (v, d) = draft(&state, &[(Key::Source, "host"), (Key::Wired, "2000")]);
    assert!(plan(&state, &v, &d).is_err());
}
#[test]
fn shared_rate_is_one_control_and_one_write() {
    let mut state = device();
    state.capabilities.polling_rates = PollingRateCapabilities::Shared {
        supported_hz: vec![125, 1000],
    };
    state.settings.configuration_source = Some(ConfigurationSource::Host);
    assert!(!specs(&state).iter().any(|s| s.key == Key::Wireless));
    let (v, d) = draft(&state, &[(Key::Wired, "125")]);
    assert_eq!(plan(&state, &v, &d).unwrap().len(), 1);
}
#[test]
fn profile_switch_is_separate_from_sensor_edits() {
    let state = device();
    let (v, d) = draft(&state, &[(Key::Source, "2"), (Key::Dpi, "1600")]);
    assert!(plan(&state, &v, &d).is_err());
    let (v, d) = draft(&state, &[(Key::Source, "2")]);
    assert!(matches!(
        plan(&state, &v, &d).unwrap()[0].command,
        RequestCommand::UseOnboardProfile { profile: 2, .. }
    ));
    let (v, d) = draft(&state, &[(Key::Source, "6")]);
    assert!(plan(&state, &v, &d).is_err());
}
#[test]
fn unsupported_settings_never_enter_write_plan() {
    let mut state = device();
    state.capabilities.surface_mode = false;
    let (v, d) = draft(&state, &[(Key::Surface, "on")]);
    assert!(plan(&state, &v, &d).is_err());
}
#[test]
fn sensor_and_lighting_validation_matches_agent() {
    let state = device();
    for value in ["0", "150", "1100"] {
        let (v, d) = draft(&state, &[(Key::Timeout, value)]);
        assert!(plan(&state, &v, &d).is_err());
    }
    let (v, d) = draft(
        &state,
        &[
            (Key::Bhop, "on"),
            (Key::Timeout, "500"),
            (Key::Lod, "low"),
            (Key::Surface, "automatic"),
            (Key::Operating, "endurance"),
        ],
    );
    assert_eq!(plan(&state, &v, &d).unwrap().len(), 4);
    let (v, d) = draft(
        &state,
        &[(Key::Lighting, "breathing"), (Key::Color, "12AB34")],
    );
    assert!(matches!(
        plan(&state, &v, &d).unwrap()[0].command,
        RequestCommand::SetLighting {
            effect: LightingEffect::Breathing {
                color: RgbColor {
                    red: 0x12,
                    green: 0xab,
                    blue: 0x34
                },
                ..
            },
            ..
        }
    ));
    for (key, value) in [
        (Key::Brightness, "101"),
        (Key::Period, "0"),
        (Key::Color, "purple"),
        (Key::Zone, "256"),
    ] {
        let (v, d) = draft(&state, &[(Key::Lighting, "breathing"), (key, value)]);
        assert!(plan(&state, &v, &d).is_err());
    }
}
#[test]
fn partial_batch_keeps_unapplied_drafts() {
    let mut state = device();
    let (submitted, pending) = draft(&state, &[(Key::Dpi, "1600"), (Key::Surface, "automatic")]);
    state.settings.dpi.as_mut().unwrap().current_x = 1600;
    let (initial, remaining) = reconcile(&state, &submitted, &pending, &[Key::Dpi]);
    assert_eq!(initial[&Key::Dpi], remaining[&Key::Dpi]);
    assert_eq!(remaining[&Key::Surface], "automatic");
    assert_eq!(initial[&Key::Surface], "off");
}
#[test]
fn failed_preflight_preserves_every_edit_and_exposes_latest_values() {
    let mut state = device();
    let (submitted, pending) = draft(&state, &[(Key::Dpi, "1600")]);
    state.settings.dpi.as_mut().unwrap().current_x = 1200;
    let (initial, remaining) = reconcile(&state, &submitted, &pending, &[]);
    assert_eq!(initial[&Key::Dpi], "1200");
    assert_eq!(remaining[&Key::Dpi], "1600");
}
#[test]
fn nickname_can_be_cleared_and_is_length_checked() {
    let state = device();
    let (v, d) = draft(&state, &[(Key::Nickname, " ")]);
    assert!(matches!(
        plan(&state, &v, &d).unwrap()[0].command,
        RequestCommand::SetDeviceNickname { nickname: None, .. }
    ));
    let long = "x".repeat(65);
    let (v, d) = draft(&state, &[(Key::Nickname, &long)]);
    assert!(plan(&state, &v, &d).is_err());
}

#[test]
fn enclosure_colors_are_validated_per_model() {
    for (name, colors) in [
        ("PRO X 2", vec!["white", "black", "cyan", "magenta"]),
        ("PRO X Wireless", vec!["white", "black", "red", "magenta"]),
        ("G305", vec!["white", "black", "lilac", "blue", "mint"]),
    ] {
        let mut state = device();
        state.device.display_name = Some(name.into());
        let appearance = specs(&state)
            .into_iter()
            .find(|s| s.key == Key::Appearance)
            .unwrap();
        assert_eq!(
            appearance
                .choices
                .iter()
                .map(|(v, _)| v.as_str())
                .collect::<Vec<_>>(),
            colors
        );
        for color in [
            "white", "black", "cyan", "magenta", "red", "blue", "lilac", "mint", "orange",
        ] {
            let (v, d) = draft(&state, &[(Key::Appearance, color)]);
            let result = plan(&state, &v, &d);
            assert_eq!(result.is_ok(), colors.contains(&color), "{name}: {color}");
            if let Ok(changes) = result {
                assert!(
                    matches!(&changes[0].command, RequestCommand::SetDeviceColor { color: actual, .. } if actual.as_str() == color)
                );
            }
        }
    }
    let mut state = device();
    state.device.display_name = Some("PRO X SUPERLIGHT 2 DEX".into());
    state.device.nickname = Some("G305".into());
    assert!(!specs(&state).iter().any(|s| s.key == Key::Appearance));
}
