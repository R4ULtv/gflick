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
    let mut state = device();
    state.settings.configuration_source = Some(ConfigurationSource::Host);
    for invalid in ["801", "0", "65536", "abc", ""] {
        let (v, d) = draft(&state, &[(Key::Dpi, invalid)]);
        assert!(plan(&state, &v, &d).is_err());
    }
    let (v, d) = draft(&state, &[(Key::Dpi, "1600")]);
    assert!(matches!(
        plan(&state, &v, &d).unwrap()[0].command,
        RequestCommand::SetDpiAxes {
            x: 1600,
            y: 800,
            ..
        }
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

#[test]
fn each_supported_model_exposes_its_live_controls_without_borrowing_others() {
    for (model, name, count) in [
        (DeviceModel::Superlight, "PRO X WIRELESS", 5),
        (DeviceModel::Superlight2, "PRO X 2", 5),
        (DeviceModel::G305, "G305", 6),
    ] {
        let mut state = device();
        state.device.display_name = Some(name.into());
        let extended = model == DeviceModel::Superlight2;
        let description = state
            .capabilities
            .onboard_profile_description
            .as_mut()
            .unwrap();
        description.button_count = count;
        description.profile_count = if model == DeviceModel::G305 { 1 } else { 5 };
        description.profile_format_id = match model {
            DeviceModel::G305 => 3,
            DeviceModel::Superlight => 4,
            DeviceModel::Superlight2 => 7,
        };
        state.capabilities.lift_off_distance = extended;
        state.capabilities.surface_mode = extended;
        state.capabilities.bunny_hopping = extended;
        state.capabilities.operating_mode_switch = model == DeviceModel::G305;
        state.capabilities.color_led_effects = model == DeviceModel::G305;
        state.capabilities.mouse_button_filter = true;
        state.settings.mouse_button_mapping = Some((1..=count).collect());
        state.settings.dpi.as_mut().unwrap().current_y = extended.then_some(800);
        if !extended {
            state.settings.polling_rate = PollingRateState::Shared { hz: 1000 };
            state.capabilities.polling_rates = PollingRateCapabilities::Shared {
                supported_hz: vec![125, 250, 500, 1000],
            };
        }
        let fields = specs(&state);
        let has = |key| fields.iter().any(|field| field.key == key);
        for key in [
            Key::Dpi,
            Key::Wired,
            Key::Source,
            Key::Nickname,
            Key::Appearance,
        ] {
            assert!(has(key), "{model:?} missing {key:?}");
        }
        for key in [
            Key::DpiY,
            Key::Wireless,
            Key::Lod,
            Key::Surface,
            Key::Bhop,
            Key::Timeout,
        ] {
            assert_eq!(has(key), extended, "{model:?}: {key:?}");
        }
        assert_eq!(has(Key::Operating), model == DeviceModel::G305);
        assert_eq!(has(Key::Lighting), model == DeviceModel::G305);
        assert_eq!(
            fields
                .iter()
                .filter(|field| matches!(field.key, Key::Button(_)))
                .count(),
            usize::from(count)
        );
        state.settings.onboard_dpi_stage = Some(0);
        assert!(specs(&state).iter().any(|field| field.key == Key::Stage));
    }
}
#[test]
fn independent_dpi_validates_both_axes_and_preserves_the_other_axis() {
    let mut state = device();
    let (values, dirty) = draft(&state, &[(Key::DpiY, "1600")]);
    assert!(plan(&state, &values, &dirty).is_err());
    state.settings.configuration_source = Some(ConfigurationSource::Host);
    let (values, dirty) = draft(&state, &[(Key::DpiY, "1600")]);
    assert!(matches!(
        plan(&state, &values, &dirty).unwrap()[0].command,
        RequestCommand::SetDpiAxes {
            x: 800,
            y: 1600,
            ..
        }
    ));
    let (values, dirty) = draft(&state, &[(Key::DpiY, "801")]);
    assert!(plan(&state, &values, &dirty).is_err());
    let mut single = state;
    single.settings.dpi.as_mut().unwrap().current_y = None;
    let (values, dirty) = draft(&single, &[(Key::Dpi, "1600")]);
    assert!(matches!(
        plan(&single, &values, &dirty).unwrap()[0].command,
        RequestCommand::SetDpi { dpi: 1600, .. }
    ));
}
#[test]
fn button_mapping_requires_host_control_and_preserves_unedited_buttons() {
    let mut state = device();
    state.capabilities.mouse_button_filter = true;
    state.settings.mouse_button_mapping = Some(vec![1, 2, 3, 4, 5, 6]);
    let (values, dirty) = draft(&state, &[(Key::Button(3), "5")]);
    assert!(plan(&state, &values, &dirty).is_err());
    let (values, dirty) = draft(&state, &[(Key::Source, "host"), (Key::Button(3), "5")]);
    let changes = plan(&state, &values, &dirty).unwrap();
    assert!(matches!(
        changes[0].command,
        RequestCommand::UseHostSettings { .. }
    ));
    assert!(
        matches!(&changes[1].command, RequestCommand::SetMouseButtonMapping { mapping, .. } if mapping == &[1,2,3,5,5,6])
    );
    for update in [(Key::Button(6), "1"), (Key::Button(0), "17")] {
        let (values, dirty) = draft(&state, &[(Key::Source, "host"), update]);
        assert!(plan(&state, &values, &dirty).is_err());
    }
}
#[test]
fn dpi_stage_selection_is_separate_and_requires_onboard_control() {
    let mut state = device();
    state.settings.onboard_dpi_stage = Some(0);
    let (values, dirty) = draft(&state, &[(Key::Stage, "3")]);
    assert!(matches!(
        plan(&state, &values, &dirty).unwrap()[0].command,
        RequestCommand::SetOnboardDpiStage { index: 3, .. }
    ));
    for changes in [
        vec![(Key::Stage, "5")],
        vec![(Key::Stage, "2"), (Key::Dpi, "1600")],
        vec![(Key::Stage, "1"), (Key::Source, "host")],
    ] {
        let (values, dirty) = draft(&state, &changes);
        assert!(plan(&state, &values, &dirty).is_err());
    }
}
