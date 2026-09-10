//! Blocking IPC adapter; the agent remains the sole owner of HID sessions.

use anyhow::{Result, bail};
use gflick_client::request;
use gflick_protocol::{DeviceState, DeviceSummary, RequestCommand, ResponseData};

#[derive(Debug)]
pub struct Snapshot {
    pub devices: Vec<DeviceSummary>,
    pub states: Vec<DeviceState>,
}

/// Reads the current device list and full state for every ready mouse.
pub fn load_snapshot() -> Result<Snapshot> {
    let ResponseData::Devices { devices } = request(RequestCommand::ListDevices)? else {
        bail!("agent returned an unexpected response to list_devices");
    };

    let states = devices
        .iter()
        .filter(|device| device.ready)
        .map(|device| {
            match request(RequestCommand::GetDevice {
                device_id: device.id.clone(),
            })? {
                ResponseData::Device { device } => Ok(*device),
                _ => bail!("agent returned an unexpected response to get_device"),
            }
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Snapshot { devices, states })
}

/// Saves the full hardware-ID order; unidentified devices remain unordered.
pub fn reorder(hardware_ids: Vec<String>) -> Result<()> {
    match request(RequestCommand::ReorderDevices { hardware_ids })? {
        ResponseData::Acknowledged => Ok(()),
        _ => bail!("agent returned an unexpected response to reorder_devices"),
    }
}

pub struct ApplyOutcome {
    pub state: Option<DeviceState>,
    pub applied: Vec<crate::settings::Key>,
    pub error: Option<String>,
}

/// Stop on the first failure and always attempt a fresh read. Writes are not an
/// atomic transaction; an error must never roll the UI back to an invented state.
pub fn apply_changes(baseline: DeviceState, changes: Vec<crate::settings::Change>) -> ApplyOutcome {
    apply_with(baseline, changes, request)
}

fn apply_with(
    baseline: DeviceState,
    changes: Vec<crate::settings::Change>,
    mut send: impl FnMut(RequestCommand) -> Result<ResponseData>,
) -> ApplyOutcome {
    let mut outcome = ApplyOutcome {
        state: None,
        applied: Vec::new(),
        error: None,
    };
    let read = || RequestCommand::GetDevice {
        device_id: baseline.device.id.clone(),
    };
    let result = (|| -> Result<()> {
        let ResponseData::Device { device: current } = send(read())? else {
            bail!("agent returned no device state");
        };
        outcome.state = Some(*current.clone());
        let mut old_settings = baseline.settings.clone();
        let mut new_settings = current.settings.clone();
        old_settings.battery = None;
        new_settings.battery = None;
        if !current.device.ready
            || baseline.device.hardware_id != current.device.hardware_id
            || old_settings != new_settings
            || baseline.capabilities != current.capabilities
            || baseline.device.nickname != current.device.nickname
            || baseline.device.color != current.device.color
        {
            bail!(
                "The device changed since it was read. Review the updated values, then apply again."
            );
        }
        for change in changes {
            let metadata = matches!(
                change.command,
                RequestCommand::SetDeviceNickname { .. } | RequestCommand::SetDeviceColor { .. }
            );
            match send(change.command)? {
                ResponseData::Device { device } => outcome.state = Some(*device),
                ResponseData::Acknowledged if metadata => {}
                _ => bail!("agent returned an unexpected write response"),
            }
            outcome.applied.extend(change.keys);
        }
        Ok(())
    })();
    if let Err(error) = result {
        outcome.error = Some(format!("{error:#}"));
    }
    match send(read()) {
        Ok(ResponseData::Device { device }) => outcome.state = Some(*device),
        other => {
            let detail = match other {
                Err(error) => format!("{error:#}"),
                _ => "unexpected agent response".into(),
            };
            outcome.error = Some(format!(
                "{}Device read-back failed: {detail}. Refresh before making more changes.",
                outcome.error.map(|e| format!("{e} ")).unwrap_or_default()
            ));
            // A write response is useful for display, but it is not a successful
            // final read. The editor keeps an error visible until refresh.
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        settings::{Change, Key},
        test_support::device,
    };
    fn response(state: &DeviceState) -> ResponseData {
        ResponseData::Device {
            device: Box::new(state.clone()),
        }
    }
    fn dpi() -> Change {
        Change {
            keys: vec![Key::Dpi],
            command: RequestCommand::SetDpi {
                device_id: "mouse".into(),
                dpi: 1600,
            },
        }
    }

    #[test]
    fn external_color_change_preserves_the_new_saved_value() {
        let baseline = device();
        let mut current = baseline.clone();
        current.device.color = gflick_protocol::DeviceColor::White;
        let outcome = apply_with(baseline, vec![dpi()], |command| {
            assert!(matches!(command, RequestCommand::GetDevice { .. }));
            Ok(response(&current))
        });
        assert!(outcome.error.is_some());
        assert!(outcome.applied.is_empty());
        assert_eq!(
            outcome.state.unwrap().device.color,
            gflick_protocol::DeviceColor::White
        );
    }

    #[test]
    fn color_acknowledgement_reads_back_saved_appearance() {
        let baseline = device();
        let mut actual = baseline.clone();
        let outcome = apply_with(
            baseline,
            vec![Change {
                keys: vec![Key::Appearance],
                command: RequestCommand::SetDeviceColor {
                    device_id: "mouse".into(),
                    color: gflick_protocol::DeviceColor::Cyan,
                },
            }],
            |command| match command {
                RequestCommand::GetDevice { .. } => Ok(response(&actual)),
                RequestCommand::SetDeviceColor { color, .. } => {
                    actual.device.color = color;
                    Ok(ResponseData::Acknowledged)
                }
                _ => panic!("color must not issue hardware commands"),
            },
        );
        assert!(outcome.error.is_none());
        assert_eq!(outcome.applied, vec![Key::Appearance]);
        assert_eq!(
            outcome.state.unwrap().device.color,
            gflick_protocol::DeviceColor::Cyan
        );
    }

    #[test]
    fn stops_failed_batch_and_reads_actual_partial_state() {
        let baseline = device();
        let mut actual = baseline.clone();
        let mut calls = Vec::new();
        let changes = vec![
            dpi(),
            Change {
                keys: vec![Key::Surface],
                command: RequestCommand::SetSurfaceMode {
                    device_id: "mouse".into(),
                    mode: gflick_protocol::SurfaceMode::On,
                },
            },
            Change {
                keys: vec![Key::Nickname],
                command: RequestCommand::SetDeviceNickname {
                    device_id: "mouse".into(),
                    nickname: Some("Desk".into()),
                },
            },
        ];
        let outcome = apply_with(baseline, changes, |command| {
            calls.push(command.clone());
            match command {
                RequestCommand::GetDevice { .. } => Ok(response(&actual)),
                RequestCommand::SetDpi { dpi, .. } => {
                    actual.settings.dpi.as_mut().unwrap().current_x = dpi;
                    Ok(response(&actual))
                }
                RequestCommand::SetSurfaceMode { .. } => bail!("simulated device failure"),
                _ => panic!("writes after failure must not execute"),
            }
        });
        assert_eq!(outcome.applied, vec![Key::Dpi]);
        assert!(outcome.error.unwrap().contains("simulated device failure"));
        assert_eq!(outcome.state.unwrap().settings.dpi.unwrap().current_x, 1600);
        assert!(matches!(
            calls.last(),
            Some(RequestCommand::GetDevice { .. })
        ));
        assert_eq!(calls.len(), 4);
    }
    #[test]
    fn concurrent_settings_change_is_not_overwritten() {
        let baseline = device();
        let mut actual = baseline.clone();
        actual.settings.dpi.as_mut().unwrap().current_x = 1200;
        let outcome = apply_with(baseline, vec![dpi()], |command| {
            assert!(matches!(command, RequestCommand::GetDevice { .. }));
            Ok(response(&actual))
        });
        assert!(outcome.applied.is_empty());
        assert!(outcome.error.unwrap().contains("changed since"));
        assert_eq!(outcome.state, Some(actual));
    }
    #[test]
    fn battery_change_does_not_block_a_write() {
        let baseline = device();
        let mut actual = baseline.clone();
        actual.settings.battery.as_mut().unwrap().percentage = 46;
        let outcome = apply_with(baseline, vec![dpi()], |command| {
            if let RequestCommand::SetDpi { dpi, .. } = command {
                actual.settings.dpi.as_mut().unwrap().current_x = dpi;
            }
            Ok(response(&actual))
        });
        assert!(outcome.error.is_none());
        assert_eq!(outcome.applied, vec![Key::Dpi]);
    }
    #[test]
    fn nickname_acknowledgement_is_followed_by_readback() {
        let baseline = device();
        let mut actual = baseline.clone();
        let outcome = apply_with(
            baseline,
            vec![Change {
                keys: vec![Key::Nickname],
                command: RequestCommand::SetDeviceNickname {
                    device_id: "mouse".into(),
                    nickname: Some("Desk".into()),
                },
            }],
            |command| {
                if let RequestCommand::SetDeviceNickname { nickname, .. } = command {
                    actual.device.nickname = nickname;
                    Ok(ResponseData::Acknowledged)
                } else {
                    Ok(response(&actual))
                }
            },
        );
        assert!(outcome.error.is_none());
        assert_eq!(
            outcome.state.unwrap().device.nickname.as_deref(),
            Some("Desk")
        );
        assert_eq!(outcome.applied, vec![Key::Nickname]);
    }
    #[test]
    fn failed_final_read_never_reports_success() {
        let baseline = device();
        let mut actual = baseline.clone();
        let mut reads = 0;
        let outcome = apply_with(baseline, vec![dpi()], |command| {
            if matches!(command, RequestCommand::GetDevice { .. }) {
                reads += 1;
                if reads == 2 {
                    bail!("disconnected");
                }
            }
            if let RequestCommand::SetDpi { dpi, .. } = command {
                actual.settings.dpi.as_mut().unwrap().current_x = dpi;
            }
            Ok(response(&actual))
        });
        assert!(outcome.error.unwrap().contains("read-back failed"));
    }
    #[test]
    fn replacement_device_on_same_receiver_is_not_written() {
        let baseline = device();
        let mut replacement = baseline.clone();
        replacement.device.hardware_id = Some("different mouse".into());
        let outcome = apply_with(baseline, vec![dpi()], |command| {
            assert!(matches!(command, RequestCommand::GetDevice { .. }));
            Ok(response(&replacement))
        });
        assert!(outcome.error.is_some());
        assert!(outcome.applied.is_empty());
    }
}
