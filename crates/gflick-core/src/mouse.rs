use anyhow::{Context, Result, bail};

use crate::{
    BatteryInfo, BunnyHoppingInfo, ColorLedEffect, ColorLedEffectSettings, ColorLedState,
    ConfigurationSource, ConnectionType, DeviceLinkStatus, DeviceMetadata, DpiInfo,
    FEATURE_ADJUSTABLE_DPI, FEATURE_BATTERY_STATUS, FEATURE_BUNNY_HOPPING,
    FEATURE_COLOR_LED_EFFECTS, FEATURE_DEVICE_INFORMATION, FEATURE_DEVICE_TYPE_AND_NAME,
    FEATURE_EXTENDED_ADJUSTABLE_DPI, FEATURE_EXTENDED_REPORT_RATE, FEATURE_MODE_STATUS,
    FEATURE_MOUSE_BUTTON_FILTER, FEATURE_ONBOARD_PROFILES, FEATURE_REPORT_RATE,
    FEATURE_UNIFIED_BATTERY, FEATURE_WIRELESS_DEVICE_STATUS, FeatureInfo, FeatureSetEntry,
    HidppSession, LiftOffDistance, ModeStatusInfo, MouseButtonFilterInfo, OnboardButtonBinding,
    OnboardDpiStage, OnboardProfile, OnboardProfileDirectoryEntry, OnboardProfileEdit,
    OnboardProfileEditProof, OnboardProfileWriteProof, OnboardProfilesDescription, OperatingMode,
    RgbColor, SurfaceMode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCapabilities {
    pub battery: bool,
    pub supported_dpi: Option<Vec<u16>>,
    pub polling_rates: PollingRateCapabilities,
    pub onboard_profiles: bool,
    pub onboard_profile_description: Option<OnboardProfilesDescription>,
    pub lift_off_distance: bool,
    pub surface_mode: bool,
    pub operating_mode_switch: bool,
    pub color_led_effects: bool,
    pub bunny_hopping: bool,
    pub mouse_button_filter: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSnapshot {
    pub battery: Option<BatteryInfo>,
    pub dpi: Option<DpiInfo>,
    pub polling_rate: PollingRateSettings,
    pub configuration_source: Option<ConfigurationSource>,
    pub mode_status: Option<ModeStatusInfo>,
    pub bunny_hopping: Option<BunnyHoppingInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollingRateSettings {
    Unsupported,
    Shared { hz: u16 },
    PerConnection { wired_hz: u16, wireless_hz: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingChange<T> {
    pub before: T,
    pub after: T,
}

#[derive(Debug, Clone, Copy)]
struct MouseFeatures {
    device_information: Option<FeatureInfo>,
    device_type_and_name: Option<FeatureInfo>,
    unified_battery: Option<FeatureInfo>,
    battery_status: Option<FeatureInfo>,
    extended_dpi: Option<FeatureInfo>,
    dpi: Option<FeatureInfo>,
    extended_report_rate: Option<FeatureInfo>,
    report_rate: Option<FeatureInfo>,
    color_led_effects: Option<FeatureInfo>,
    onboard_profiles: Option<FeatureInfo>,
    mode_status: Option<FeatureInfo>,
    bunny_hopping: Option<FeatureInfo>,
    mouse_button_filter: Option<FeatureInfo>,
}

/// High-level Logitech mouse API used by the probe and future background agent.
pub struct MouseDevice {
    session: HidppSession,
    features: MouseFeatures,
}

impl MouseDevice {
    pub fn discover(session: HidppSession) -> Result<Self> {
        let wireless_device_status = session.feature(FEATURE_WIRELESS_DEVICE_STATUS)?;
        session.set_wireless_status_feature(wireless_device_status);
        let features = MouseFeatures {
            device_information: session.feature(FEATURE_DEVICE_INFORMATION)?,
            device_type_and_name: session.feature(FEATURE_DEVICE_TYPE_AND_NAME)?,
            unified_battery: session.feature(FEATURE_UNIFIED_BATTERY)?,
            battery_status: session.feature(FEATURE_BATTERY_STATUS)?,
            extended_dpi: session.feature(FEATURE_EXTENDED_ADJUSTABLE_DPI)?,
            dpi: session.feature(FEATURE_ADJUSTABLE_DPI)?,
            extended_report_rate: session.feature(FEATURE_EXTENDED_REPORT_RATE)?,
            report_rate: session.feature(FEATURE_REPORT_RATE)?,
            color_led_effects: session
                .feature(FEATURE_COLOR_LED_EFFECTS)?
                .filter(|feature| feature.flags & 0x60 == 0),
            onboard_profiles: session.feature(FEATURE_ONBOARD_PROFILES)?,
            mode_status: session.feature(FEATURE_MODE_STATUS)?,
            bunny_hopping: session.feature(FEATURE_BUNNY_HOPPING)?,
            mouse_button_filter: session.feature(FEATURE_MOUSE_BUTTON_FILTER)?,
        };
        Ok(Self { session, features })
    }

    pub fn session(&self) -> &HidppSession {
        &self.session
    }

    /// Reads buffered connection notifications without transmitting to the mouse.
    pub fn poll_link_status(&self) -> Result<Option<DeviceLinkStatus>> {
        self.session.poll_link_status()
    }

    pub fn feature_set(&self) -> Result<Vec<FeatureSetEntry>> {
        self.session.feature_set()
    }

    pub fn metadata(&self) -> Result<Option<DeviceMetadata>> {
        let (Some(information_feature), Some(name_feature)) = (
            self.features.device_information,
            self.features.device_type_and_name,
        ) else {
            return Ok(None);
        };

        let information = self.session.device_information(information_feature)?;
        let firmware = (0..information.entity_count)
            .map(|entity| self.session.firmware_entity(information_feature, entity))
            .collect::<Result<Vec<_>>>()?;
        let serial_number = (information_feature.version >= 4
            && information.capabilities & 0x01 != 0)
            .then(|| self.session.device_serial_number(information_feature))
            .transpose()?;

        Ok(Some(DeviceMetadata {
            name: self.session.device_name(name_feature)?,
            device_type: self.session.device_type(name_feature)?,
            information,
            firmware,
            serial_number,
        }))
    }

    /// Reads the device-reported mouse name without the additional firmware metadata.
    pub fn name(&self) -> Result<Option<String>> {
        self.features
            .device_type_and_name
            .map(|feature| self.session.device_name(feature))
            .transpose()
    }

    /// Returns an opaque identity suitable for associating persisted preferences with
    /// a physical mouse. Unlike the transport/session ID used by `DeviceManager`, this
    /// value is derived from the HID++ unit ID and does not depend on a USB path,
    /// receiver, or connection type.
    pub fn hardware_id(&self) -> Result<Option<String>> {
        let Some(feature) = self.features.device_information else {
            return Ok(None);
        };
        let information = self.session.device_information(feature)?;
        Ok(hardware_id_from_unit_id(information.unit_id))
    }

    pub fn capabilities(&self) -> Result<DeviceCapabilities> {
        let extended_dpi_capabilities = self
            .features
            .extended_dpi
            .map(|feature| self.session.extended_dpi_capabilities(feature))
            .transpose()?;
        let supported_dpi = if let Some(feature) = self.features.extended_dpi {
            Some(self.session.supported_extended_adjustable_dpi(feature)?)
        } else if let Some(feature) = self.features.dpi {
            Some(self.session.supported_adjustable_dpi(feature)?)
        } else {
            None
        };

        let polling_rates = if let Some(feature) = self.features.extended_report_rate {
            PollingRateCapabilities::PerConnection {
                wired_hz: self
                    .session
                    .extended_report_rate(feature, ConnectionType::Wired)?
                    .supported_hz,
                wireless_hz: self
                    .session
                    .extended_report_rate(feature, ConnectionType::GamingWireless)?
                    .supported_hz,
            }
        } else if let Some(feature) = self.features.report_rate {
            PollingRateCapabilities::Shared {
                supported_hz: self.session.report_rate(feature)?.supported_hz,
            }
        } else {
            PollingRateCapabilities::Unsupported
        };
        let mode_status = self
            .features
            .mode_status
            .map(|feature| self.session.mode_status(feature))
            .transpose()?;

        Ok(DeviceCapabilities {
            battery: self.features.unified_battery.is_some()
                || self.features.battery_status.is_some(),
            supported_dpi,
            polling_rates,
            onboard_profiles: self.features.onboard_profiles.is_some(),
            onboard_profile_description: self
                .features
                .onboard_profiles
                .map(|feature| self.session.onboard_profiles_description(feature))
                .transpose()?,
            lift_off_distance: extended_dpi_capabilities
                .is_some_and(|capabilities| capabilities.has_lod),
            surface_mode: mode_status.is_some_and(|mode| mode.surface_mode.is_some()),
            operating_mode_switch: mode_status
                .is_some_and(ModeStatusInfo::supports_software_operating_mode),
            color_led_effects: self.features.color_led_effects.is_some(),
            bunny_hopping: self.features.bunny_hopping.is_some(),
            mouse_button_filter: self.features.mouse_button_filter.is_some(),
        })
    }

    pub fn settings(&self) -> Result<SettingsSnapshot> {
        let dpi = self.dpi()?;
        let polling_rate = if let Some(feature) = self.features.extended_report_rate {
            PollingRateSettings::PerConnection {
                wired_hz: self
                    .session
                    .extended_report_rate(feature, ConnectionType::Wired)?
                    .current_hz,
                wireless_hz: self
                    .session
                    .extended_report_rate(feature, ConnectionType::GamingWireless)?
                    .current_hz,
            }
        } else if let Some(feature) = self.features.report_rate {
            PollingRateSettings::Shared {
                hz: self.session.report_rate(feature)?.current_hz,
            }
        } else {
            PollingRateSettings::Unsupported
        };

        Ok(SettingsSnapshot {
            battery: self.battery()?,
            dpi,
            polling_rate,
            configuration_source: self.configuration_source()?,
            mode_status: self
                .features
                .mode_status
                .map(|feature| self.session.mode_status(feature))
                .transpose()?,
            bunny_hopping: self
                .features
                .bunny_hopping
                .map(|feature| self.session.bunny_hopping(feature))
                .transpose()?,
        })
    }

    /// Reads only the battery feature, avoiding the extra HID++ traffic of a
    /// complete settings snapshot.
    pub fn battery(&self) -> Result<Option<BatteryInfo>> {
        if let Some(feature) = self.features.unified_battery {
            return self.session.unified_battery(feature).map(Some);
        }
        self.features
            .battery_status
            .map(|feature| self.session.legacy_battery(feature))
            .transpose()
    }

    pub fn dpi(&self) -> Result<Option<DpiInfo>> {
        if let Some(feature) = self.features.extended_dpi {
            let capabilities = self.session.extended_dpi_capabilities(feature)?;
            return self
                .session
                .extended_adjustable_dpi(feature, capabilities)
                .map(Some);
        }
        self.features
            .dpi
            .map(|feature| self.session.adjustable_dpi(feature))
            .transpose()
    }

    pub fn configuration_source(&self) -> Result<Option<ConfigurationSource>> {
        self.features
            .onboard_profiles
            .map(|feature| {
                self.session
                    .onboard_profiles(feature)
                    .map(|profiles| profiles.configuration_source())
            })
            .transpose()
    }

    pub fn color_led_state(&self) -> Result<Option<ColorLedState>> {
        self.features
            .color_led_effects
            .map(|feature| self.session.color_led_state(feature))
            .transpose()
    }

    pub fn set_color_led_effect(
        &self,
        zone_index: u8,
        effect: ColorLedEffect,
    ) -> Result<Option<ColorLedEffectSettings>> {
        let feature = self
            .features
            .color_led_effects
            .context("mouse does not expose Color LED Effects feature 0x8070")?;
        validate_color_led_effect(effect)?;
        let state = self.session.color_led_state(feature)?;
        let zone = state
            .zones
            .iter()
            .find(|zone| zone.index == zone_index)
            .with_context(|| format!("LED zone {zone_index} does not exist"))?;
        let supported_effect = zone
            .effects
            .iter()
            .find(|supported| supported.effect_id == effect.effect_id());
        let Some(supported_effect) = supported_effect else {
            bail!(
                "LED zone {zone_index} does not advertise the requested {} effect",
                crate::color_led_effect_name(effect.effect_id())
            );
        };

        self.session.set_color_led_software_control(feature, true)?;
        let controlled = self.session.color_led_state(feature)?;
        if controlled.software_control != Some(true) {
            bail!("failed to acquire software control of Color LED Effects");
        }
        self.session
            .set_color_led_effect(feature, zone_index, supported_effect.index, effect)?;

        if state.info.extended_capabilities & 0x0002 != 0 {
            return Ok(None);
        }
        let settings = self
            .session
            .color_led_effect_settings(feature, zone_index)?;
        if settings.zone_effect_index != supported_effect.index {
            bail!(
                "LED effect verification failed: requested zone effect index {}, read back {}",
                supported_effect.index,
                settings.zone_effect_index
            );
        }
        match effect {
            ColorLedEffect::Disabled => {}
            ColorLedEffect::Fixed { color }
                if settings.params[0..3] != [color.red, color.green, color.blue] =>
            {
                bail!(
                    "fixed LED color verification failed: requested {color}, read back #{:02x}{:02x}{:02x}",
                    settings.params[0],
                    settings.params[1],
                    settings.params[2]
                )
            }
            ColorLedEffect::Cycling {
                period_ms,
                brightness,
            } if u16::from_be_bytes([settings.params[5], settings.params[6]]) != period_ms
                || led_brightness(settings.params[7]) != brightness =>
            {
                bail!(
                    "cycling LED verification failed: requested {period_ms} ms/{brightness}%, read back {} ms/{}%",
                    u16::from_be_bytes([settings.params[5], settings.params[6]]),
                    led_brightness(settings.params[7])
                )
            }
            ColorLedEffect::Breathing {
                color,
                period_ms,
                brightness,
            } if settings.params[0..3] != [color.red, color.green, color.blue]
                || u16::from_be_bytes([settings.params[3], settings.params[4]]) != period_ms
                || led_brightness(settings.params[6]) != brightness =>
            {
                bail!(
                    "breathing LED verification failed: requested {color}/{period_ms} ms/{brightness}%, read back {}/{} ms/{}%",
                    RgbColor {
                        red: settings.params[0],
                        green: settings.params[1],
                        blue: settings.params[2]
                    },
                    u16::from_be_bytes([settings.params[3], settings.params[4]]),
                    led_brightness(settings.params[6])
                )
            }
            _ => {}
        }
        Ok(Some(settings))
    }

    /// Return Color LED Effects control to the device firmware.
    ///
    /// Effects set through [`Self::set_color_led_effect`] are deliberately
    /// volatile. Releasing software control lets the mouse resume its normal
    /// firmware-controlled indicator behavior without writing onboard flash.
    pub fn release_color_led_control(&self) -> Result<()> {
        let feature = self
            .features
            .color_led_effects
            .context("mouse does not expose Color LED Effects feature 0x8070")?;
        self.session
            .set_color_led_software_control(feature, false)?;
        let state = self.session.color_led_state(feature)?;
        if state.software_control == Some(true) {
            bail!("failed to return Color LED Effects control to the firmware");
        }
        Ok(())
    }

    pub fn stored_profiles(&self) -> Result<Vec<OnboardProfile>> {
        let feature = self
            .features
            .onboard_profiles
            .context("mouse does not expose Onboard Profiles feature 0x8100")?;
        let description = self.session.onboard_profiles_description(feature)?;

        let directory = self
            .session
            .read_onboard_profile_sector(feature, 0x0000, description.sector_size)
            .ok()
            .and_then(|data| parse_profile_directory(&data, description.profile_count).ok())
            .filter(|entries| !entries.is_empty())
            .unwrap_or_else(|| {
                (0..description.factory_profile_count)
                    .map(|index| OnboardProfileDirectoryEntry {
                        sector: 0x0101 + u16::from(index),
                        enabled: true,
                    })
                    .collect()
            });

        directory
            .into_iter()
            .map(|entry| {
                let data = self.session.read_onboard_profile_sector(
                    feature,
                    entry.sector,
                    description.sector_size,
                )?;
                parse_stored_profile(
                    description.profile_format_id,
                    entry,
                    &data,
                    description.button_count,
                )
            })
            .collect()
    }

    /// Applies one validated, typed edit and saves the profile transactionally.
    pub fn edit_stored_profile(
        &self,
        profile_number: usize,
        edit: OnboardProfileEdit,
    ) -> Result<SettingChange<OnboardProfile>> {
        let (description, mut profile) = self.read_writable_profile(profile_number)?;
        let before = profile.clone();
        let capabilities = self.capabilities()?;
        apply_profile_edit(
            &mut profile,
            description.profile_format_id,
            &capabilities,
            edit,
        )?;
        let after = self.write_stored_profile(profile_number, &profile)?;
        Ok(SettingChange { before, after })
    }

    /// Enables or disables a profile by transactionally updating its directory entry.
    pub fn set_stored_profile_enabled(
        &self,
        profile_number: usize,
        enabled: bool,
    ) -> Result<SettingChange<bool>> {
        let feature = self
            .features
            .onboard_profiles
            .context("mouse does not expose Onboard Profiles feature 0x8100")?;
        let description = self.session.onboard_profiles_description(feature)?;
        let directory =
            self.session
                .read_onboard_profile_sector(feature, 0x0000, description.sector_size)?;
        if !onboard_profile_crc_valid(&directory) {
            bail!("refusing to update profile states because the directory CRC is invalid");
        }
        let entries = parse_profile_directory(&directory, description.profile_count)?;
        let entry = *entries.get(profile_number.checked_sub(1).with_context(|| {
            "profile number is one-based; expected 1 or greater".to_owned()
        })?).with_context(|| {
            format!(
                "profile {profile_number} does not exist; writable directory contains {} profiles",
                entries.len()
            )
        })?;
        let before = entry.enabled;
        if before == enabled {
            return Ok(SettingChange {
                before,
                after: before,
            });
        }

        if enabled {
            let profile_data = self.session.read_onboard_profile_sector(
                feature,
                entry.sector,
                description.sector_size,
            )?;
            if !onboard_profile_crc_valid(&profile_data) {
                bail!(
                    "refusing to enable profile {profile_number}: sector 0x{:04x} has an invalid CRC",
                    entry.sector
                );
            }
        } else {
            if matches!(
                self.configuration_source()?,
                Some(ConfigurationSource::Onboard {
                    active_profile: Some(active)
                }) if active == entry.sector
            ) {
                bail!(
                    "refusing to disable active onboard profile {profile_number}; activate another profile first"
                );
            }
        }

        let desired = encode_profile_enabled_state(
            &directory,
            description.profile_count,
            profile_number,
            enabled,
        )?;
        self.write_profile_sector_transactional(
            feature,
            &description,
            0x0000,
            &directory,
            &desired,
        )?;

        let verified =
            self.session
                .read_onboard_profile_sector(feature, 0x0000, description.sector_size)?;
        let verified_entries = parse_profile_directory(&verified, description.profile_count)?;
        let after = verified_entries
            .get(profile_number - 1)
            .context("profile disappeared after directory write")?
            .enabled;
        if after != enabled || !onboard_profile_crc_valid(&verified) {
            bail!("profile {profile_number} enabled-state verification failed");
        }
        Ok(SettingChange { before, after })
    }

    /// Writes one parsed profile using optimistic concurrency, CRC regeneration,
    /// read-back verification, and automatic rollback.
    ///
    /// `profile_number` is one-based, matching the number shown by the probe.
    /// Changing whether a profile is enabled is intentionally not handled here,
    /// because that flag lives in the profile directory sector.
    pub fn write_stored_profile(
        &self,
        profile_number: usize,
        profile: &OnboardProfile,
    ) -> Result<OnboardProfile> {
        let (feature, description, entry) = self.writable_profile_entry(profile_number)?;
        if profile.sector != entry.sector {
            bail!(
                "profile {profile_number} is sector 0x{:04x}, but the supplied profile is sector 0x{:04x}",
                entry.sector,
                profile.sector
            );
        }
        if profile.enabled != entry.enabled {
            bail!("profile enable/disable changes require a transactional directory update");
        }

        let current = self.session.read_onboard_profile_sector(
            feature,
            entry.sector,
            description.sector_size,
        )?;
        if current != profile.raw_data {
            bail!(
                "profile {profile_number} changed on the device after it was read; reload it before saving"
            );
        }
        if !onboard_profile_crc_valid(&current) {
            bail!(
                "refusing to overwrite profile {profile_number}: its current sector CRC is invalid"
            );
        }

        let desired = encode_stored_profile(
            description.profile_format_id,
            profile,
            description.button_count,
        )?;
        if desired != current {
            self.write_profile_sector_transactional(
                feature,
                &description,
                entry.sector,
                &current,
                &desired,
            )?;
        }

        let verified = self.session.read_onboard_profile_sector(
            feature,
            entry.sector,
            description.sector_size,
        )?;
        parse_stored_profile(
            description.profile_format_id,
            entry,
            &verified,
            description.button_count,
        )
    }

    fn read_writable_profile(
        &self,
        profile_number: usize,
    ) -> Result<(OnboardProfilesDescription, OnboardProfile)> {
        let (feature, description, entry) = self.writable_profile_entry(profile_number)?;
        let data = self.session.read_onboard_profile_sector(
            feature,
            entry.sector,
            description.sector_size,
        )?;
        if !onboard_profile_crc_valid(&data) {
            bail!("profile {profile_number} has an invalid CRC");
        }
        let profile = parse_stored_profile(
            description.profile_format_id,
            entry,
            &data,
            description.button_count,
        )?;
        Ok((description, profile))
    }

    /// Performs an explicitly destructive protocol proof by writing an identical,
    /// valid image back to a disabled profile and verifying every byte.
    pub fn prove_disabled_profile_write(
        &self,
        profile_number: usize,
    ) -> Result<OnboardProfileWriteProof> {
        let (feature, description, entry) = self.writable_profile_entry(profile_number)?;
        if entry.enabled {
            bail!(
                "refusing write proof on enabled profile {profile_number}; select a disabled profile"
            );
        }

        let original = self.session.read_onboard_profile_sector(
            feature,
            entry.sector,
            description.sector_size,
        )?;
        if !onboard_profile_crc_valid(&original) {
            bail!("refusing write proof on profile {profile_number}: sector CRC is invalid");
        }
        let parsed = parse_stored_profile(
            description.profile_format_id,
            entry,
            &original,
            description.button_count,
        )?;
        let encoded = encode_stored_profile(
            description.profile_format_id,
            &parsed,
            description.button_count,
        )?;
        if encoded != original {
            bail!("byte-preserving encoder proof failed before writing profile {profile_number}");
        }

        self.write_profile_sector_transactional(
            feature,
            &description,
            entry.sector,
            &original,
            &encoded,
        )?;
        let verified = self.session.read_onboard_profile_sector(
            feature,
            entry.sector,
            description.sector_size,
        )?;
        if verified != original {
            bail!("profile {profile_number} changed after a successful identical-data write");
        }

        let crc = u16::from_be_bytes([verified[verified.len() - 2], verified[verified.len() - 1]]);
        Ok(OnboardProfileWriteProof {
            profile_number,
            sector: entry.sector,
            bytes_verified: verified.len(),
            crc,
        })
    }

    /// Temporarily changes a disabled profile name, verifies the changed sector,
    /// and restores the exact original bytes before returning.
    pub fn prove_reversible_profile_name_edit(
        &self,
        profile_number: usize,
        temporary_name: &str,
    ) -> Result<OnboardProfileEditProof> {
        let (feature, description, entry) = self.writable_profile_entry(profile_number)?;
        if entry.enabled {
            bail!(
                "refusing reversible edit proof on enabled profile {profile_number}; select a disabled profile"
            );
        }
        let original = self.session.read_onboard_profile_sector(
            feature,
            entry.sector,
            description.sector_size,
        )?;
        if !onboard_profile_crc_valid(&original) {
            bail!("profile {profile_number} has an invalid source CRC");
        }
        let mut edited = parse_stored_profile(
            description.profile_format_id,
            entry,
            &original,
            description.button_count,
        )?;
        let original_name = edited.name.clone();
        edited.name = Some(temporary_name.to_owned());
        let temporary = encode_stored_profile(
            description.profile_format_id,
            &edited,
            description.button_count,
        )?;
        if temporary == original {
            bail!("temporary profile name does not change the stored sector");
        }

        self.write_profile_sector_transactional(
            feature,
            &description,
            entry.sector,
            &original,
            &temporary,
        )?;

        let changed_verification = self
            .session
            .read_onboard_profile_sector(feature, entry.sector, description.sector_size)
            .and_then(|changed| {
                if changed != temporary || !onboard_profile_crc_valid(&changed) {
                    bail!("temporary profile-name bytes failed verification");
                }
                let parsed = parse_stored_profile(
                    description.profile_format_id,
                    entry,
                    &changed,
                    description.button_count,
                )?;
                if parsed.name.as_deref() != Some(temporary_name) {
                    bail!("temporary profile name failed semantic verification");
                }
                Ok(())
            });

        let restore_result = self.write_profile_sector_transactional(
            feature,
            &description,
            entry.sector,
            &temporary,
            &original,
        );
        if let Err(restore_error) = restore_result {
            let recovery_write =
                self.session
                    .write_onboard_profile_sector(feature, entry.sector, &original);
            let recovery_read = self.session.read_onboard_profile_sector(
                feature,
                entry.sector,
                description.sector_size,
            );
            if !recovery_read
                .as_ref()
                .is_ok_and(|recovered| recovered == &original)
            {
                bail!(
                    "profile {profile_number} restoration failed ({restore_error:#}); emergency recovery write: {}; recovery read: {}",
                    recovery_write
                        .err()
                        .map(|error| format!("{error:#}"))
                        .unwrap_or_else(|| "sent".to_owned()),
                    recovery_read
                        .err()
                        .map(|error| format!("{error:#}"))
                        .unwrap_or_else(|| "bytes did not match".to_owned())
                );
            }
            bail!(
                "profile {profile_number} normal restoration reported an error ({restore_error:#}); exact original bytes were recovered by the emergency path"
            );
        }

        changed_verification?;
        let restored = self.session.read_onboard_profile_sector(
            feature,
            entry.sector,
            description.sector_size,
        )?;
        if restored != original || !onboard_profile_crc_valid(&restored) {
            bail!("profile {profile_number} was not restored byte-for-byte");
        }
        let original_crc =
            u16::from_be_bytes([original[original.len() - 2], original[original.len() - 1]]);
        Ok(OnboardProfileEditProof {
            profile_number,
            sector: entry.sector,
            original_name,
            temporary_name: temporary_name.to_owned(),
            bytes_restored: restored.len(),
            original_crc,
        })
    }

    fn writable_profile_entry(
        &self,
        profile_number: usize,
    ) -> Result<(
        FeatureInfo,
        OnboardProfilesDescription,
        OnboardProfileDirectoryEntry,
    )> {
        if profile_number == 0 {
            bail!("profile number is one-based; expected 1 or greater");
        }
        let feature = self
            .features
            .onboard_profiles
            .context("mouse does not expose Onboard Profiles feature 0x8100")?;
        let description = self.session.onboard_profiles_description(feature)?;
        let directory_data =
            self.session
                .read_onboard_profile_sector(feature, 0x0000, description.sector_size)?;
        if !onboard_profile_crc_valid(&directory_data) {
            bail!("refusing to write profiles because the writable directory CRC is invalid");
        }
        let directory = parse_profile_directory(&directory_data, description.profile_count)?;
        let entry = *directory.get(profile_number - 1).with_context(|| {
            format!(
                "profile {profile_number} does not exist; writable directory contains {} profiles",
                directory.len()
            )
        })?;
        if entry.sector == 0 || entry.sector > 0x00ff {
            bail!(
                "profile {profile_number} points to non-writable sector 0x{:04x}",
                entry.sector
            );
        }
        Ok((feature, description, entry))
    }

    fn write_profile_sector_transactional(
        &self,
        feature: FeatureInfo,
        description: &OnboardProfilesDescription,
        sector: u16,
        backup: &[u8],
        desired: &[u8],
    ) -> Result<()> {
        let expected_len = usize::from(description.sector_size);
        if backup.len() != expected_len || desired.len() != expected_len {
            bail!(
                "profile sector length mismatch: expected {expected_len}, backup {}, desired {}",
                backup.len(),
                desired.len()
            );
        }
        if !onboard_profile_crc_valid(desired) {
            bail!("refusing to write profile sector 0x{sector:04x} with an invalid CRC");
        }

        let write_result = self
            .session
            .write_onboard_profile_sector(feature, sector, desired);
        let observed_result =
            self.session
                .read_onboard_profile_sector(feature, sector, description.sector_size);

        if write_result.is_ok()
            && observed_result
                .as_ref()
                .is_ok_and(|observed| observed == desired)
        {
            return Ok(());
        }

        // Some firmware reports an error from MemoryWriteEnd after committing.
        // A changed sector that exactly matches the desired bytes is authoritative.
        if desired != backup
            && observed_result
                .as_ref()
                .is_ok_and(|observed| observed == desired)
        {
            return Ok(());
        }

        let original_problem = match (&write_result, &observed_result) {
            (Err(error), _) => format!("write protocol failed: {error:#}"),
            (_, Err(error)) => format!("write-back read failed: {error:#}"),
            (Ok(_), Ok(_)) => "read-back bytes did not match the requested image".to_owned(),
        };

        let already_unchanged = observed_result
            .as_ref()
            .is_ok_and(|observed| observed == backup);
        if already_unchanged {
            bail!(
                "profile sector 0x{sector:04x} write was not accepted ({original_problem}); original bytes remain intact"
            );
        }

        let rollback_write = self
            .session
            .write_onboard_profile_sector(feature, sector, backup);
        let rollback_read =
            self.session
                .read_onboard_profile_sector(feature, sector, description.sector_size);
        if rollback_read
            .as_ref()
            .is_ok_and(|observed| observed == backup)
        {
            bail!(
                "profile sector 0x{sector:04x} write failed ({original_problem}); original bytes were restored{}",
                rollback_write
                    .err()
                    .map(|error| format!(" despite a completion error: {error:#}"))
                    .unwrap_or_default()
            );
        }

        let rollback_problem = rollback_read
            .err()
            .map(|error| format!("rollback read failed: {error:#}"))
            .or_else(|| {
                rollback_write
                    .err()
                    .map(|error| format!("rollback write failed: {error:#}"))
            })
            .unwrap_or_else(|| "rollback verification bytes did not match the backup".to_owned());
        bail!(
            "profile sector 0x{sector:04x} write failed ({original_problem}) and automatic rollback could not be verified ({rollback_problem})"
        )
    }

    pub fn mouse_button_filter(&self) -> Result<Option<MouseButtonFilterInfo>> {
        self.features
            .mouse_button_filter
            .map(|feature| self.session.mouse_button_filter(feature))
            .transpose()
    }

    pub fn set_mouse_button_filter(&self, mapping: &[u8]) -> Result<SettingChange<Vec<u8>>> {
        let feature = self
            .features
            .mouse_button_filter
            .context("mouse does not expose Mouse Button Filter feature 0x8110")?;
        if matches!(
            self.configuration_source()?,
            Some(ConfigurationSource::Onboard { .. })
        ) {
            bail!(
                "host button mapping can only be changed while host/local settings control the mouse"
            );
        }
        let before = self.session.mouse_button_filter(feature)?;
        if mapping.len() != usize::from(before.button_count) {
            bail!(
                "expected {} mouse-button mappings, got {}",
                before.button_count,
                mapping.len()
            );
        }
        self.session.set_mouse_button_filter(feature, mapping)?;
        let after = self.session.mouse_button_filter(feature)?;
        if after.mapping != mapping {
            bail!(
                "mouse-button mapping verification failed: requested {mapping:?}, read back {:?}",
                after.mapping
            );
        }
        Ok(SettingChange {
            before: before.mapping,
            after: after.mapping,
        })
    }

    pub fn set_host_control(&self) -> Result<()> {
        let Some(feature) = self.features.onboard_profiles else {
            bail!("mouse does not expose Onboard Profiles feature 0x8100");
        };
        self.session.disable_onboard_profiles(feature)
    }

    pub fn activate_onboard_profile(&self, profile: u16) -> Result<()> {
        let Some(feature) = self.features.onboard_profiles else {
            bail!("mouse does not expose Onboard Profiles feature 0x8100");
        };
        self.session.activate_onboard_profile(feature, profile)
    }

    pub fn current_onboard_dpi_stage(&self) -> Result<Option<u8>> {
        self.features
            .onboard_profiles
            .map(|feature| self.session.current_onboard_dpi_index(feature))
            .transpose()
    }

    pub fn set_current_onboard_dpi_stage(&self, index: u8) -> Result<SettingChange<u8>> {
        let feature = self
            .features
            .onboard_profiles
            .context("mouse does not expose Onboard Profiles feature 0x8100")?;
        if !matches!(
            self.configuration_source()?,
            Some(ConfigurationSource::Onboard { .. })
        ) {
            bail!("an onboard profile must be active before selecting one of its DPI stages");
        }
        let before = self.session.current_onboard_dpi_index(feature)?;
        self.session.set_current_onboard_dpi_index(feature, index)?;
        let after = self.session.current_onboard_dpi_index(feature)?;
        if after != index {
            bail!("DPI-stage verification failed: requested {index}, read back {after}");
        }
        Ok(SettingChange { before, after })
    }

    /// Sets independently validated X/Y values without changing lift-off distance.
    pub fn set_dpi_axes(&self, x: u16, y: u16) -> Result<SettingChange<DpiInfo>> {
        let feature = self
            .features
            .extended_dpi
            .context("mouse does not expose independent X/Y DPI")?;
        let capabilities = self.session.extended_dpi_capabilities(feature)?;
        if !capabilities.has_y {
            bail!("mouse does not expose independent Y-axis DPI");
        }
        let supported = self.session.supported_extended_adjustable_dpi(feature)?;
        validate_supported("X-axis DPI", x, &supported)?;
        validate_supported("Y-axis DPI", y, &supported)?;
        let before = self
            .session
            .extended_adjustable_dpi(feature, capabilities)?;
        self.session
            .set_extended_adjustable_dpi(feature, capabilities, x, y, before.lod)?;
        let after = self
            .session
            .extended_adjustable_dpi(feature, capabilities)?;
        if after.current_x != x || after.current_y != Some(y) {
            bail!("X/Y DPI read-back did not match the requested values");
        }
        Ok(SettingChange { before, after })
    }

    pub fn set_dpi(&self, requested: u16) -> Result<SettingChange<DpiInfo>> {
        if let Some(feature) = self.features.extended_dpi {
            let capabilities = self.session.extended_dpi_capabilities(feature)?;
            let before = self
                .session
                .extended_adjustable_dpi(feature, capabilities)?;
            let supported = self.session.supported_extended_adjustable_dpi(feature)?;
            validate_supported("DPI", requested, &supported)?;
            self.session.set_extended_adjustable_dpi(
                feature,
                capabilities,
                requested,
                requested,
                before.lod,
            )?;
            let after = self
                .session
                .extended_adjustable_dpi(feature, capabilities)?;
            if after.current_x != requested
                || capabilities.has_y && after.current_y != Some(requested)
            {
                bail!(
                    "DPI verification failed: requested {requested}, read back X={}, Y={:?}",
                    after.current_x,
                    after.current_y
                );
            }
            return Ok(SettingChange { before, after });
        }

        if let Some(feature) = self.features.dpi {
            let before = self.session.adjustable_dpi(feature)?;
            let supported = self.session.supported_adjustable_dpi(feature)?;
            validate_supported("DPI", requested, &supported)?;
            self.session.set_adjustable_dpi(feature, requested)?;
            let after = self.session.adjustable_dpi(feature)?;
            if after.current_x != requested {
                bail!(
                    "DPI verification failed: requested {requested}, read back {}",
                    after.current_x
                );
            }
            return Ok(SettingChange { before, after });
        }

        bail!("mouse exposes neither Adjustable DPI feature 0x2202 nor 0x2201")
    }

    pub fn set_lift_off_distance(
        &self,
        requested: LiftOffDistance,
    ) -> Result<SettingChange<LiftOffDistance>> {
        let Some(feature) = self.features.extended_dpi else {
            bail!("mouse does not expose Extended Adjustable DPI feature 0x2202");
        };
        let capabilities = self.session.extended_dpi_capabilities(feature)?;
        if !capabilities.has_lod {
            bail!("mouse's Extended Adjustable DPI feature does not support LOD");
        }
        let before = self
            .session
            .extended_adjustable_dpi(feature, capabilities)?;
        let previous = before
            .lod
            .context("mouse advertised LOD but did not return its current value")?;
        self.session.set_extended_adjustable_dpi(
            feature,
            capabilities,
            before.current_x,
            before.current_y.unwrap_or(before.current_x),
            Some(requested),
        )?;
        let after = self
            .session
            .extended_adjustable_dpi(feature, capabilities)?;
        if after.lod != Some(requested) {
            bail!(
                "LOD verification failed: requested {requested}, read back {:?}",
                after.lod
            );
        }
        Ok(SettingChange {
            before: previous,
            after: requested,
        })
    }

    pub fn set_surface_mode(&self, requested: SurfaceMode) -> Result<SettingChange<SurfaceMode>> {
        let Some(feature) = self.features.mode_status else {
            bail!("mouse does not expose Mode Status feature 0x8090");
        };
        let before = self.session.mode_status(feature)?;
        let previous = before
            .surface_mode
            .context("mouse's Mode Status feature does not advertise Surface Mode")?;
        self.session.set_surface_mode(feature, requested)?;
        let after = self.session.mode_status(feature)?;
        if after.surface_mode != Some(requested) {
            bail!(
                "surface-mode verification failed: requested {requested}, read back {:?}",
                after.surface_mode
            );
        }
        Ok(SettingChange {
            before: previous,
            after: requested,
        })
    }

    pub fn set_operating_mode(
        &self,
        requested: OperatingMode,
    ) -> Result<SettingChange<OperatingMode>> {
        let feature = self
            .features
            .mode_status
            .context("mouse does not expose Mode Status feature 0x8090")?;
        let before = self.session.mode_status(feature)?;
        if !before.supports_software_operating_mode() {
            bail!("mouse does not advertise a software performance/endurance switch");
        }
        self.session.set_operating_mode(feature, requested)?;
        let after = self.session.mode_status(feature)?;
        if after.operating_mode() != requested {
            bail!(
                "operating-mode verification failed: requested {requested}, read back {}",
                after.operating_mode()
            );
        }
        Ok(SettingChange {
            before: before.operating_mode(),
            after: after.operating_mode(),
        })
    }

    pub fn bunny_hopping(&self) -> Result<Option<BunnyHoppingInfo>> {
        self.features
            .bunny_hopping
            .map(|feature| self.session.bunny_hopping(feature))
            .transpose()
    }

    pub fn set_bunny_hopping(
        &self,
        enabled: bool,
        timeout_ms: u16,
    ) -> Result<SettingChange<BunnyHoppingInfo>> {
        let Some(feature) = self.features.bunny_hopping else {
            bail!("mouse does not expose Bunny Hopping feature 0x80e0");
        };
        validate_bunny_hopping_timeout(timeout_ms)?;
        if matches!(
            self.configuration_source()?,
            Some(ConfigurationSource::Onboard { .. })
        ) {
            bail!("BHOP can only be changed while host/local settings control the mouse");
        }

        let before = self.session.bunny_hopping(feature)?;
        self.session
            .set_bunny_hopping(feature, enabled, timeout_ms)?;
        let after = self.session.bunny_hopping(feature)?;
        if after.enabled != enabled || enabled && after.timeout_ms != timeout_ms {
            bail!(
                "BHOP verification failed: requested enabled={enabled}, timeout={timeout_ms} ms; read back enabled={}, timeout={} ms",
                after.enabled,
                after.timeout_ms
            );
        }
        Ok(SettingChange { before, after })
    }

    /// Sets the live polling rate. `connection` must match the active transport.
    pub fn set_polling_rate(
        &self,
        connection: ConnectionType,
        requested: u16,
    ) -> Result<SettingChange<u16>> {
        if let Some(feature) = self.features.extended_report_rate {
            let before = self.session.extended_report_rate(feature, connection)?;
            validate_supported("polling rate", requested, &before.supported_hz)?;
            self.session.set_extended_report_rate(feature, requested)?;
            let after = self.session.extended_report_rate(feature, connection)?;
            if after.current_hz != requested {
                bail!(
                    "polling-rate verification failed: requested {requested} Hz, read back {} Hz",
                    after.current_hz
                );
            }
            return Ok(SettingChange {
                before: before.current_hz,
                after: after.current_hz,
            });
        }

        if let Some(feature) = self.features.report_rate {
            let before = self.session.report_rate(feature)?;
            validate_supported("polling rate", requested, &before.supported_hz)?;
            self.session.set_report_rate(feature, requested)?;
            let after = self.session.report_rate(feature)?;
            if after.current_hz != requested {
                bail!(
                    "polling-rate verification failed: requested {requested} Hz, read back {} Hz",
                    after.current_hz
                );
            }
            return Ok(SettingChange {
                before: before.current_hz,
                after: after.current_hz,
            });
        }

        bail!("mouse exposes neither Report Rate feature 0x8061 nor 0x8060")
    }
}

fn hardware_id_from_unit_id(unit_id: [u8; 4]) -> Option<String> {
    if unit_id == [0; 4] || unit_id == [u8::MAX; 4] {
        return None;
    }
    Some(format!(
        "046d:unit:{:02x}{:02x}{:02x}{:02x}",
        unit_id[0], unit_id[1], unit_id[2], unit_id[3]
    ))
}

fn apply_profile_edit(
    profile: &mut OnboardProfile,
    format: u8,
    capabilities: &DeviceCapabilities,
    edit: OnboardProfileEdit,
) -> Result<()> {
    match edit {
        OnboardProfileEdit::Name(name) => {
            if name.as_deref() == Some("") {
                bail!("profile name must not be empty; use no name to restore the default");
            }
            profile.name = name;
        }
        OnboardProfileEdit::DpiStage {
            index,
            x,
            y,
            lod,
            make_default,
            make_shift,
        } => {
            let supported = capabilities
                .supported_dpi
                .as_deref()
                .context("mouse does not advertise onboard DPI values")?;
            validate_supported("DPI", x, supported)?;
            let stage = profile
                .dpi_stages
                .get_mut(usize::from(index))
                .with_context(|| format!("DPI stage {index} does not exist"))?;
            match format {
                0x01..=0x05 => {
                    if y.is_some() || lod.is_some() {
                        bail!("legacy profile format 0x{format:02x} has no Y-axis or LOD fields");
                    }
                    *stage = OnboardDpiStage {
                        x,
                        y: None,
                        lod: None,
                    };
                }
                0x06 | 0x07 => {
                    let y = y.unwrap_or(x);
                    validate_supported("Y-axis DPI", y, supported)?;
                    *stage = OnboardDpiStage {
                        x,
                        y: Some(y),
                        lod: Some(
                            lod.or(stage.lod)
                                .context("extended profile DPI stage has no valid LOD")?,
                        ),
                    };
                }
                _ => bail!("unsupported onboard profile format 0x{format:02x}"),
            }
            if make_default {
                profile.default_dpi_index = index;
            }
            if make_shift {
                profile.shifted_dpi_index = index;
            }
        }
        OnboardProfileEdit::DefaultDpiStage(index) => {
            validate_profile_dpi_index(profile, index)?;
            profile.default_dpi_index = index;
        }
        OnboardProfileEdit::ShiftDpiStage(index) => {
            validate_profile_dpi_index(profile, index)?;
            profile.shifted_dpi_index = index;
        }
        OnboardProfileEdit::PollingRate { connection, hz } => match &capabilities.polling_rates {
            PollingRateCapabilities::Unsupported => {
                bail!("mouse does not advertise configurable polling rates")
            }
            PollingRateCapabilities::Shared { supported_hz } => {
                validate_supported("polling rate", hz, supported_hz)?;
                profile.wired_hz = Some(hz);
                profile.wireless_hz = Some(hz);
            }
            PollingRateCapabilities::PerConnection {
                wired_hz,
                wireless_hz,
            } => match connection {
                ConnectionType::Wired => {
                    validate_supported("wired polling rate", hz, wired_hz)?;
                    profile.wired_hz = Some(hz);
                }
                ConnectionType::GamingWireless => {
                    validate_supported("wireless polling rate", hz, wireless_hz)?;
                    profile.wireless_hz = Some(hz);
                }
            },
        },
        OnboardProfileEdit::PowerTimeouts {
            save_seconds,
            off_seconds,
        } => {
            profile.power_save_timeout = save_seconds;
            profile.power_off_timeout = off_seconds;
        }
        OnboardProfileEdit::BunnyHopping { timeout_ms } => {
            if format != 0x07 {
                bail!("onboard profile format 0x{format:02x} does not support BHOP");
            }
            if let Some(timeout_ms) = timeout_ms {
                validate_bunny_hopping_timeout(timeout_ms)?;
            }
            profile.bunny_hopping_timeout_ms = timeout_ms;
        }
        OnboardProfileEdit::Button { button, action } => {
            let index = button
                .checked_sub(1)
                .context("button number is one-based; expected 1 or greater")?;
            let button_count = profile.buttons.len();
            let binding = profile
                .buttons
                .get_mut(usize::from(index))
                .with_context(|| {
                    format!(
                        "button {button} does not exist; profile has {} buttons",
                        button_count
                    )
                })?;
            *binding = OnboardButtonBinding::from_action(action)?;
        }
    }
    Ok(())
}

fn validate_profile_dpi_index(profile: &OnboardProfile, index: u8) -> Result<()> {
    let stage = profile
        .dpi_stages
        .get(usize::from(index))
        .with_context(|| format!("DPI stage {index} does not exist"))?;
    if stage.x == 0 {
        bail!("DPI stage {index} is disabled and cannot be selected");
    }
    Ok(())
}

fn encode_profile_enabled_state(
    directory: &[u8],
    maximum_entries: u8,
    profile_number: usize,
    enabled: bool,
) -> Result<Vec<u8>> {
    if !onboard_profile_crc_valid(directory) {
        bail!("profile directory CRC is invalid");
    }
    let entries = parse_profile_directory(directory, maximum_entries)?;
    let index = profile_number
        .checked_sub(1)
        .context("profile number is one-based; expected 1 or greater")?;
    let entry = entries.get(index).with_context(|| {
        format!(
            "profile {profile_number} does not exist; writable directory contains {} profiles",
            entries.len()
        )
    })?;
    if !enabled && entry.enabled && entries.iter().filter(|entry| entry.enabled).count() <= 1 {
        bail!("refusing to disable the last enabled onboard profile");
    }

    let mut desired = directory.to_vec();
    desired[index * 4 + 2] = u8::from(enabled);
    write_onboard_crc(&mut desired)?;
    Ok(desired)
}

fn parse_profile_directory(
    data: &[u8],
    maximum_entries: u8,
) -> Result<Vec<OnboardProfileDirectoryEntry>> {
    let mut entries = Vec::new();
    for chunk in data.chunks_exact(4).take(usize::from(maximum_entries)) {
        let sector = u16::from_be_bytes([chunk[0], chunk[1]]);
        if sector == 0xffff {
            break;
        }
        if sector == 0 {
            continue;
        }
        entries.push(OnboardProfileDirectoryEntry {
            sector,
            enabled: chunk[2] != 0,
        });
    }
    Ok(entries)
}

fn parse_stored_profile(
    format: u8,
    entry: OnboardProfileDirectoryEntry,
    data: &[u8],
    button_count: u8,
) -> Result<OnboardProfile> {
    if data.len() < 255 {
        bail!(
            "profile sector 0x{:04x} is only {} bytes",
            entry.sector,
            data.len()
        );
    }

    match format {
        0x06 | 0x07 => parse_extended_stored_profile(format, entry, data, button_count),
        0x01..=0x05 => parse_legacy_stored_profile(entry, data, button_count),
        _ => bail!("unsupported onboard profile format 0x{format:02x}"),
    }
}

fn parse_legacy_stored_profile(
    entry: OnboardProfileDirectoryEntry,
    data: &[u8],
    button_count: u8,
) -> Result<OnboardProfile> {
    let dpi_stages = (0..5)
        .map(|index| {
            let offset = 3 + index * 2;
            OnboardDpiStage {
                x: u16::from_le_bytes([data[offset], data[offset + 1]]),
                y: None,
                lod: None,
            }
        })
        .collect();
    let wired_hz = (data[0] != 0).then(|| 1000 / u16::from(data[0]));

    Ok(OnboardProfile {
        sector: entry.sector,
        enabled: entry.enabled,
        crc_valid: onboard_profile_crc_valid(data),
        name: decode_profile_name(&data[160..208], true),
        wired_hz,
        wireless_hz: wired_hz,
        default_dpi_index: data[1],
        shifted_dpi_index: data[2],
        dpi_stages,
        bunny_hopping_timeout_ms: None,
        power_save_timeout: u16::from_le_bytes([data[28], data[29]]),
        power_off_timeout: u16::from_le_bytes([data[30], data[31]]),
        buttons: parse_profile_buttons(data, 32, button_count),
        raw_data: data.to_vec(),
    })
}

fn parse_extended_stored_profile(
    format: u8,
    entry: OnboardProfileDirectoryEntry,
    data: &[u8],
    button_count: u8,
) -> Result<OnboardProfile> {
    let dpi_stages = (0..5)
        .map(|index| {
            let offset = 4 + index * 5;
            let lod = match data[offset + 4] {
                1 => Some(LiftOffDistance::Low),
                2 => Some(LiftOffDistance::Medium),
                3 => Some(LiftOffDistance::High),
                _ => None,
            };
            OnboardDpiStage {
                x: u16::from_le_bytes([data[offset], data[offset + 1]]),
                y: Some(u16::from_le_bytes([data[offset + 2], data[offset + 3]])),
                lod,
            }
        })
        .collect();

    Ok(OnboardProfile {
        sector: entry.sector,
        enabled: entry.enabled,
        crc_valid: onboard_profile_crc_valid(data),
        name: decode_profile_name(&data[160..208], true),
        wired_hz: crate::extended_rate_hz(data[1]),
        wireless_hz: crate::extended_rate_hz(data[0]),
        default_dpi_index: data[2],
        shifted_dpi_index: data[3],
        dpi_stages,
        bunny_hopping_timeout_ms: (format == 0x07 && !matches!(data[37], 0 | 0xff))
            .then(|| u16::from(data[37]) * 10),
        power_save_timeout: u16::from_le_bytes([data[44], data[45]]),
        power_off_timeout: u16::from_le_bytes([data[46], data[47]]),
        buttons: parse_profile_buttons(data, 48, button_count),
        raw_data: data.to_vec(),
    })
}

fn parse_profile_buttons(data: &[u8], offset: usize, count: u8) -> Vec<OnboardButtonBinding> {
    (0..usize::from(count).min(16))
        .map(|index| {
            let start = offset + index * 4;
            OnboardButtonBinding {
                raw: data[start..start + 4]
                    .try_into()
                    .expect("four-byte button binding"),
            }
        })
        .collect()
}

fn encode_stored_profile(
    format: u8,
    profile: &OnboardProfile,
    button_count: u8,
) -> Result<Vec<u8>> {
    if !matches!(format, 0x01..=0x07) {
        bail!("unsupported onboard profile format 0x{format:02x}");
    }
    if profile.raw_data.len() < 255 {
        bail!(
            "profile sector 0x{:04x} is only {} bytes",
            profile.sector,
            profile.raw_data.len()
        );
    }
    if !onboard_profile_crc_valid(&profile.raw_data) {
        bail!(
            "profile sector 0x{:04x} has an invalid source CRC",
            profile.sector
        );
    }

    let entry = OnboardProfileDirectoryEntry {
        sector: profile.sector,
        enabled: profile.enabled,
    };
    let original = parse_stored_profile(format, entry, &profile.raw_data, button_count)?;
    let mut data = profile.raw_data.clone();

    if profile.default_dpi_index > 4 || profile.shifted_dpi_index > 4 {
        bail!("default and shifted DPI indices must be in 0..=4");
    }
    if profile.dpi_stages.len() != 5 {
        bail!(
            "onboard profiles require exactly five DPI stages, got {}",
            profile.dpi_stages.len()
        );
    }
    let expected_buttons = usize::from(button_count).min(16);
    if profile.buttons.len() != expected_buttons {
        bail!(
            "onboard profile requires {expected_buttons} button bindings, got {}",
            profile.buttons.len()
        );
    }

    if profile.name != original.name {
        encode_profile_name(&mut data[160..208], profile.name.as_deref())?;
    }

    match format {
        0x01..=0x05 => {
            if profile.wired_hz != profile.wireless_hz {
                bail!("legacy onboard profiles use one shared wired/wireless polling rate");
            }
            if profile.wired_hz != original.wired_hz {
                data[0] = match profile.wired_hz {
                    Some(hz) => crate::legacy_rate_code(hz).with_context(|| {
                        format!("{hz} Hz cannot be stored in a legacy onboard profile")
                    })?,
                    None => 0,
                };
            }
            if profile.default_dpi_index != original.default_dpi_index {
                data[1] = profile.default_dpi_index;
            }
            if profile.shifted_dpi_index != original.shifted_dpi_index {
                data[2] = profile.shifted_dpi_index;
            }
            for (index, (stage, previous)) in profile
                .dpi_stages
                .iter()
                .zip(&original.dpi_stages)
                .enumerate()
            {
                if stage.y.is_some() || stage.lod.is_some() {
                    bail!("legacy onboard DPI stages cannot contain Y-axis or LOD values");
                }
                if stage != previous {
                    let offset = 3 + index * 2;
                    data[offset..offset + 2].copy_from_slice(&stage.x.to_le_bytes());
                }
            }
            if profile.bunny_hopping_timeout_ms.is_some() {
                bail!("legacy onboard profile format 0x{format:02x} does not support BHOP");
            }
            if profile.power_save_timeout != original.power_save_timeout {
                data[28..30].copy_from_slice(&profile.power_save_timeout.to_le_bytes());
            }
            if profile.power_off_timeout != original.power_off_timeout {
                data[30..32].copy_from_slice(&profile.power_off_timeout.to_le_bytes());
            }
            encode_changed_buttons(&mut data, 32, &profile.buttons, &original.buttons);
        }
        0x06 | 0x07 => {
            if profile.wireless_hz != original.wireless_hz {
                let hz = profile
                    .wireless_hz
                    .context("extended onboard profiles require a wireless polling rate")?;
                data[0] = crate::extended_rate_code(hz).with_context(|| {
                    format!("{hz} Hz cannot be stored in an extended onboard profile")
                })?;
            }
            if profile.wired_hz != original.wired_hz {
                let hz = profile
                    .wired_hz
                    .context("extended onboard profiles require a wired polling rate")?;
                data[1] = crate::extended_rate_code(hz).with_context(|| {
                    format!("{hz} Hz cannot be stored in an extended onboard profile")
                })?;
            }
            if profile.default_dpi_index != original.default_dpi_index {
                data[2] = profile.default_dpi_index;
            }
            if profile.shifted_dpi_index != original.shifted_dpi_index {
                data[3] = profile.shifted_dpi_index;
            }
            for (index, (stage, previous)) in profile
                .dpi_stages
                .iter()
                .zip(&original.dpi_stages)
                .enumerate()
            {
                if stage != previous {
                    let y = stage
                        .y
                        .context("extended onboard DPI stages require a Y-axis value")?;
                    let lod = stage
                        .lod
                        .context("extended onboard DPI stages require an LOD value")?;
                    let offset = 4 + index * 5;
                    data[offset..offset + 2].copy_from_slice(&stage.x.to_le_bytes());
                    data[offset + 2..offset + 4].copy_from_slice(&y.to_le_bytes());
                    data[offset + 4] = lod as u8 + 1;
                }
            }
            if format == 0x06 && profile.bunny_hopping_timeout_ms.is_some() {
                bail!("onboard profile format 0x06 does not support BHOP");
            }
            if format == 0x07
                && profile.bunny_hopping_timeout_ms != original.bunny_hopping_timeout_ms
            {
                data[37] = match profile.bunny_hopping_timeout_ms {
                    Some(timeout) => {
                        validate_bunny_hopping_timeout(timeout)?;
                        u8::try_from(timeout / 10)
                            .context("BHOP timeout does not fit in the profile field")?
                    }
                    None => 0,
                };
            }
            if profile.power_save_timeout != original.power_save_timeout {
                data[44..46].copy_from_slice(&profile.power_save_timeout.to_le_bytes());
            }
            if profile.power_off_timeout != original.power_off_timeout {
                data[46..48].copy_from_slice(&profile.power_off_timeout.to_le_bytes());
            }
            encode_changed_buttons(&mut data, 48, &profile.buttons, &original.buttons);
        }
        _ => unreachable!("profile format was validated above"),
    }

    let content_len = data.len() - 2;
    let crc = hidpp_crc_ccitt(&data[..content_len]);
    data[content_len..].copy_from_slice(&crc.to_be_bytes());
    Ok(data)
}

fn encode_changed_buttons(
    data: &mut [u8],
    offset: usize,
    buttons: &[OnboardButtonBinding],
    original: &[OnboardButtonBinding],
) {
    for (index, (binding, previous)) in buttons.iter().zip(original).enumerate() {
        if binding != previous {
            let start = offset + index * 4;
            data[start..start + 4].copy_from_slice(&binding.raw);
        }
    }
}

fn encode_profile_name(output: &mut [u8], name: Option<&str>) -> Result<()> {
    output.fill(0xff);
    let Some(name) = name else {
        return Ok(());
    };
    let words = name.encode_utf16().collect::<Vec<_>>();
    if words.len() > output.len() / 2 {
        bail!(
            "profile name is too long: {} UTF-16 code units, maximum {}",
            words.len(),
            output.len() / 2
        );
    }
    for (index, word) in words.iter().enumerate() {
        let start = index * 2;
        output[start..start + 2].copy_from_slice(&word.to_le_bytes());
    }
    if words.len() < output.len() / 2 {
        let terminator = words.len() * 2;
        output[terminator..terminator + 2].copy_from_slice(&0_u16.to_le_bytes());
    }
    Ok(())
}

fn decode_profile_name(data: &[u8], utf16: bool) -> Option<String> {
    if data.iter().all(|byte| *byte == 0xff || *byte == 0x00) {
        return None;
    }
    if utf16 {
        let words = data
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .take_while(|word| !matches!(word, 0 | 0xffff));
        let name = char::decode_utf16(words)
            .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>();
        (!name.is_empty()).then_some(name)
    } else {
        let end = data
            .iter()
            .position(|byte| matches!(byte, 0 | 0xff))
            .unwrap_or(data.len());
        let name = String::from_utf8_lossy(&data[..end]).to_string();
        (!name.is_empty()).then_some(name)
    }
}

fn onboard_profile_crc_valid(data: &[u8]) -> bool {
    if data.len() < 2 {
        return false;
    }
    let expected = u16::from_be_bytes([data[data.len() - 2], data[data.len() - 1]]);
    hidpp_crc_ccitt(&data[..data.len() - 2]) == expected
}

fn write_onboard_crc(data: &mut [u8]) -> Result<u16> {
    if data.len() < 2 {
        bail!("onboard sector is too small to contain a CRC");
    }
    let content_len = data.len() - 2;
    let crc = hidpp_crc_ccitt(&data[..content_len]);
    data[content_len..].copy_from_slice(&crc.to_be_bytes());
    Ok(crc)
}

fn hidpp_crc_ccitt(data: &[u8]) -> u16 {
    let mut crc = 0xffff_u16;
    for byte in data {
        let temp = (crc >> 8) ^ u16::from(*byte);
        crc <<= 8;
        let mut quick = temp ^ (temp >> 4);
        crc ^= quick;
        quick <<= 5;
        crc ^= quick;
        quick <<= 7;
        crc ^= quick;
    }
    crc
}

fn validate_supported(name: &str, requested: u16, supported: &[u16]) -> Result<()> {
    if !supported.contains(&requested) {
        bail!(
            "unsupported {name} {requested}; device advertises {}",
            supported
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn validate_bunny_hopping_timeout(timeout_ms: u16) -> Result<()> {
    if !(100..=1000).contains(&timeout_ms) || timeout_ms % 100 != 0 {
        bail!("BHOP timeout must be 100..=1000 ms in 100 ms steps");
    }
    Ok(())
}

fn validate_color_led_effect(effect: ColorLedEffect) -> Result<()> {
    let (period_ms, brightness) = match effect {
        ColorLedEffect::Disabled | ColorLedEffect::Fixed { .. } => return Ok(()),
        ColorLedEffect::Cycling {
            period_ms,
            brightness,
        }
        | ColorLedEffect::Breathing {
            period_ms,
            brightness,
            ..
        } => (period_ms, brightness),
    };
    if period_ms == 0 {
        bail!("LED effect period must be greater than zero milliseconds");
    }
    if !(1..=100).contains(&brightness) {
        bail!("LED brightness must be in 1..=100 percent");
    }
    Ok(())
}

fn led_brightness(raw: u8) -> u8 {
    if raw == 0 { 100 } else { raw }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_hardware_id_from_valid_unit_id() {
        assert_eq!(
            hardware_id_from_unit_id([0x10, 0x77, 0xe6, 0x9f]),
            Some("046d:unit:1077e69f".to_owned())
        );
        assert_eq!(hardware_id_from_unit_id([0; 4]), None);
        assert_eq!(hardware_id_from_unit_id([u8::MAX; 4]), None);
    }

    #[test]
    fn rejects_values_outside_capability_list() {
        let error = validate_supported("polling rate", 3000, &[1000, 2000, 4000]).unwrap_err();
        assert!(error.to_string().contains("1000, 2000, 4000"));
    }

    #[test]
    fn validates_bunny_hopping_timeout_range_and_steps() {
        assert!(validate_bunny_hopping_timeout(100).is_ok());
        assert!(validate_bunny_hopping_timeout(1000).is_ok());
        assert!(validate_bunny_hopping_timeout(0).is_err());
        assert!(validate_bunny_hopping_timeout(150).is_err());
        assert!(validate_bunny_hopping_timeout(1100).is_err());
    }

    #[test]
    fn decodes_utf16_profile_names() {
        let bytes = [b'P', 0, b'r', 0, b'o', 0, 0, 0];
        assert_eq!(decode_profile_name(&bytes, true).as_deref(), Some("Pro"));
        assert_eq!(decode_profile_name(&[0xff; 8], true), None);
    }

    #[test]
    fn parses_bhop_profile_without_treating_erased_byte_as_timeout() {
        let mut data = vec![0xff; 255];
        data[0] = 3;
        data[1] = 3;
        data[2] = 0;
        data[3] = 1;
        data[4..6].copy_from_slice(&800_u16.to_le_bytes());
        data[6..8].copy_from_slice(&800_u16.to_le_bytes());
        data[8] = 3;
        data[44..46].copy_from_slice(&60_u16.to_le_bytes());
        data[46..48].copy_from_slice(&300_u16.to_le_bytes());
        let profile = parse_stored_profile(
            0x07,
            OnboardProfileDirectoryEntry {
                sector: 1,
                enabled: true,
            },
            &data,
            0,
        )
        .unwrap();
        assert_eq!(profile.wired_hz, Some(1000));
        assert_eq!(profile.wireless_hz, Some(1000));
        assert_eq!(profile.dpi_stages[0].x, 800);
        assert_eq!(profile.dpi_stages[0].lod, Some(LiftOffDistance::High));
        assert_eq!(profile.bunny_hopping_timeout_ms, None);
        assert_eq!(profile.power_off_timeout, 300);
    }

    #[test]
    fn validates_hidpp_profile_crc() {
        let mut data = vec![0x55; 255];
        let crc = hidpp_crc_ccitt(&data[..253]);
        data[253..].copy_from_slice(&crc.to_be_bytes());
        assert!(onboard_profile_crc_valid(&data));
        data[10] ^= 1;
        assert!(!onboard_profile_crc_valid(&data));
    }

    #[test]
    fn legacy_profile_encoder_is_byte_preserving() {
        let mut data = vec![0xff; 255];
        data[0] = 1;
        data[1] = 2;
        data[2] = 1;
        for (index, dpi) in [400_u16, 800, 1600, 3200, 6400].into_iter().enumerate() {
            let offset = 3 + index * 2;
            data[offset..offset + 2].copy_from_slice(&dpi.to_le_bytes());
        }
        data[28..30].copy_from_slice(&60_u16.to_le_bytes());
        data[30..32].copy_from_slice(&300_u16.to_le_bytes());
        data[32..36].copy_from_slice(&[0x80, 0x01, 0x00, 0x01]);
        encode_profile_name(&mut data[160..208], Some("Legacy profile")).unwrap();
        write_test_crc(&mut data);

        let profile = parse_stored_profile(
            0x04,
            OnboardProfileDirectoryEntry {
                sector: 5,
                enabled: false,
            },
            &data,
            1,
        )
        .unwrap();
        assert_eq!(encode_stored_profile(0x04, &profile, 1).unwrap(), data);

        let g305_profile = parse_stored_profile(
            0x03,
            OnboardProfileDirectoryEntry {
                sector: 1,
                enabled: true,
            },
            &data,
            1,
        )
        .unwrap();
        assert_eq!(encode_stored_profile(0x03, &g305_profile, 1).unwrap(), data);
    }

    #[test]
    fn extended_profile_encoder_is_byte_preserving_and_preserves_unknown_bytes() {
        let mut data = vec![0xff; 255];
        data[0] = 4;
        data[1] = 3;
        data[2] = 0;
        data[3] = 1;
        for (index, dpi) in [800_u16, 1200, 1600, 2400, 3200].into_iter().enumerate() {
            let offset = 4 + index * 5;
            data[offset..offset + 2].copy_from_slice(&dpi.to_le_bytes());
            data[offset + 2..offset + 4].copy_from_slice(&dpi.to_le_bytes());
            data[offset + 4] = 2;
        }
        data[35] = 0x5a;
        data[37] = 10;
        data[44..46].copy_from_slice(&60_u16.to_le_bytes());
        data[46..48].copy_from_slice(&300_u16.to_le_bytes());
        data[48..52].copy_from_slice(&[0x80, 0x01, 0x00, 0x01]);
        encode_profile_name(&mut data[160..208], Some("Extended profile")).unwrap();
        write_test_crc(&mut data);

        let mut profile = parse_stored_profile(
            0x07,
            OnboardProfileDirectoryEntry {
                sector: 5,
                enabled: false,
            },
            &data,
            1,
        )
        .unwrap();
        assert_eq!(encode_stored_profile(0x07, &profile, 1).unwrap(), data);

        profile.dpi_stages[0].x = 850;
        profile.dpi_stages[0].y = Some(850);
        let edited = encode_stored_profile(0x07, &profile, 1).unwrap();
        assert_eq!(edited[35], 0x5a);
        assert_eq!(u16::from_le_bytes([edited[4], edited[5]]), 850);
        assert!(onboard_profile_crc_valid(&edited));
    }

    #[test]
    fn encodes_supported_non_macro_button_actions() {
        assert_eq!(
            OnboardButtonBinding::from_action(crate::OnboardButtonAction::MouseButtons(0x0008))
                .unwrap()
                .raw,
            [0x80, 0x01, 0x00, 0x08]
        );
        assert_eq!(
            OnboardButtonBinding::from_action(crate::OnboardButtonAction::Keyboard {
                modifiers: 0x02,
                usage: 0x04,
            })
            .unwrap()
            .raw,
            [0x80, 0x02, 0x02, 0x04]
        );
        assert_eq!(
            OnboardButtonBinding::from_action(crate::OnboardButtonAction::Consumer {
                usage: 0x00e9,
            })
            .unwrap()
            .raw,
            [0x80, 0x03, 0x00, 0xe9]
        );
        assert!(
            OnboardButtonBinding::from_action(crate::OnboardButtonAction::MouseButtons(0)).is_err()
        );
    }

    #[test]
    fn typed_edits_validate_legacy_and_extended_profile_shapes() {
        let capabilities = DeviceCapabilities {
            battery: false,
            supported_dpi: Some(vec![400, 800, 1600]),
            polling_rates: PollingRateCapabilities::PerConnection {
                wired_hz: vec![500, 1000],
                wireless_hz: vec![1000, 2000],
            },
            onboard_profiles: true,
            onboard_profile_description: None,
            lift_off_distance: true,
            surface_mode: false,
            operating_mode_switch: false,
            color_led_effects: false,
            bunny_hopping: false,
            mouse_button_filter: false,
        };

        let mut legacy_data = vec![0xff; 255];
        legacy_data[0] = 1;
        legacy_data[1] = 0;
        legacy_data[2] = 1;
        for index in 0..5 {
            let offset = 3 + index * 2;
            legacy_data[offset..offset + 2].copy_from_slice(&400_u16.to_le_bytes());
        }
        write_test_crc(&mut legacy_data);
        let mut legacy = parse_stored_profile(
            0x04,
            OnboardProfileDirectoryEntry {
                sector: 1,
                enabled: true,
            },
            &legacy_data,
            0,
        )
        .unwrap();
        assert!(
            apply_profile_edit(
                &mut legacy,
                0x04,
                &capabilities,
                OnboardProfileEdit::DpiStage {
                    index: 0,
                    x: 800,
                    y: Some(800),
                    lod: None,
                    make_default: false,
                    make_shift: false,
                },
            )
            .is_err()
        );

        let mut extended_data = vec![0xff; 255];
        extended_data[0] = 4;
        extended_data[1] = 3;
        for index in 0..5 {
            let offset = 4 + index * 5;
            extended_data[offset..offset + 2].copy_from_slice(&400_u16.to_le_bytes());
            extended_data[offset + 2..offset + 4].copy_from_slice(&400_u16.to_le_bytes());
            extended_data[offset + 4] = 2;
        }
        write_test_crc(&mut extended_data);
        let mut extended = parse_stored_profile(
            0x07,
            OnboardProfileDirectoryEntry {
                sector: 2,
                enabled: false,
            },
            &extended_data,
            0,
        )
        .unwrap();
        apply_profile_edit(
            &mut extended,
            0x07,
            &capabilities,
            OnboardProfileEdit::DpiStage {
                index: 2,
                x: 1600,
                y: None,
                lod: Some(LiftOffDistance::High),
                make_default: true,
                make_shift: false,
            },
        )
        .unwrap();
        assert_eq!(extended.dpi_stages[2].x, 1600);
        assert_eq!(extended.dpi_stages[2].y, Some(1600));
        assert_eq!(extended.dpi_stages[2].lod, Some(LiftOffDistance::High));
        assert_eq!(extended.default_dpi_index, 2);
        apply_profile_edit(
            &mut extended,
            0x07,
            &capabilities,
            OnboardProfileEdit::BunnyHopping {
                timeout_ms: Some(100),
            },
        )
        .unwrap();
        assert_eq!(extended.bunny_hopping_timeout_ms, Some(100));
        assert!(
            apply_profile_edit(
                &mut legacy,
                0x04,
                &capabilities,
                OnboardProfileEdit::BunnyHopping {
                    timeout_ms: Some(100),
                },
            )
            .is_err()
        );
    }

    #[test]
    fn directory_edits_regenerate_crc_and_keep_one_profile_enabled() {
        let mut directory = vec![0xff; 255];
        directory[0..4].copy_from_slice(&[0x00, 0x01, 0x01, 0x00]);
        directory[4..8].copy_from_slice(&[0x00, 0x02, 0x00, 0x00]);
        write_test_crc(&mut directory);

        assert!(encode_profile_enabled_state(&directory, 2, 1, false).is_err());
        let two_enabled = encode_profile_enabled_state(&directory, 2, 2, true).unwrap();
        assert!(onboard_profile_crc_valid(&two_enabled));
        assert_eq!(two_enabled[6], 1);

        let second_only = encode_profile_enabled_state(&two_enabled, 2, 1, false).unwrap();
        assert!(onboard_profile_crc_valid(&second_only));
        assert_eq!(second_only[2], 0);
        assert_eq!(second_only[6], 1);
        assert!(encode_profile_enabled_state(&second_only, 2, 2, false).is_err());
    }

    fn write_test_crc(data: &mut [u8]) {
        let content_len = data.len() - 2;
        let crc = hidpp_crc_ccitt(&data[..content_len]);
        data[content_len..].copy_from_slice(&crc.to_be_bytes());
    }
}
