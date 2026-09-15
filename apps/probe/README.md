# gflick probe

`gflick-probe` is the low-level Logitech HID/HID++ diagnostic and configuration utility used while developing gflick. It can inspect protocol features, exercise live settings, and perform guarded onboard-profile writes.

The probe is not the normal user interface. Use the [`gflick` CLI](../cli/README.md) for day-to-day configuration through the agent. The probe opens HID interfaces directly and is intended for controlled hardware investigation.

## Before running it

- Fully exit Logitech G HUB so it does not compete for the same interfaces.
- Stop `gflick-agent`; the probe and agent must not own the same device concurrently.
- Begin with read-only commands and confirm that you selected the expected device.
- Record current values before trying a reversible live-setting command.
- Treat onboard-profile writes as hardware-sensitive operations.

## Build and inspect commands

From the workspace root:

```text
cargo build -p gflick-probe
cargo run -q -p gflick-probe -- --help
```

Command-specific help includes the supported flags and safety confirmations:

```text
cargo run -q -p gflick-probe -- set-polling-rate --help
cargo run -q -p gflick-probe -- set-profile-name --help
```

## Start with read-only discovery

List matching HID interfaces and group them into physical devices:

```text
cargo run -p gflick-probe -- list
cargo run -p gflick-probe -- devices
```

Then inspect a selected target:

```text
cargo run -p gflick-probe -- probe --index 046d:c54d --device-index 1
cargo run -p gflick-probe -- features --index 046d:c54d --device-index 1
cargo run -p gflick-probe -- profiles --index 046d:c54d --device-index 1
```

Replace the selector values with those reported for your hardware.

## Device selectors

Most commands share the following targeting options:

- `--index <INDEX>` selects either a numeric entry from `list` or a USB ID such as `046d:c54d`.
- `--device-index <DEVICE_INDEX>` selects a device behind a receiver. Direct connections commonly use `ff`; receiver slots commonly start at `1`.
- `--timeout-ms <MILLISECONDS>` controls how long the probe waits for protocol replies and defaults to 1500 ms.

USB IDs can match more than one interface or receiver. Use `list`, `devices`, and a read-only probe before issuing a mutation.

## Command groups

### Read-only inspection

- `list` — enumerate matching HID interfaces.
- `devices` — group interfaces by physical device.
- `probe` — collect model, transport, battery, and protocol information.
- `features` — enumerate supported HID++ features.
- `profiles` — read onboard-profile metadata and entries.

### Live and reversible settings

These commands change active device state without using the guarded profile-flash workflow:

- `set-dpi` and `set-dpi-stage`
- `set-lift-off-distance` and `set-surface-mode`
- `set-operating-mode`
- `set-lighting` and `use-firmware-lighting`
- `set-bhop` (also available as `set-bunny-hopping`)
- `set-polling-rate`
- `use-host-settings` and `use-onboard-profile`

Support is capability-driven. A command can be valid for one model or connection type and unavailable for another. Read the command help, record the current value, and restore it after a test when possible.

### Guarded onboard-profile writes

The following commands can write persistent onboard-profile data:

- `prove-profile-write`
- `prove-profile-name-edit`
- `set-profile-name`
- `set-profile-dpi`
- `set-profile-polling-rate`
- `set-profile-power-timeouts`
- `set-profile-bhop`
- `set-profile-button`
- `set-profile-enabled`

They require the explicit `--confirm-flash-write` acknowledgement. That flag is a safety gate, not a guarantee that a particular model or profile layout is safe to modify.

The profile writer preserves unmodified bytes, recomputes integrity data, reads the result back, and attempts rollback when verification fails. Even with those protections, use flash-writing commands only on hardware and profile formats already validated in the [feature matrix](../../docs/feature-matrix.md).

## Hardware safety rules

- Do not guess feature IDs, register layouts, profile addresses, or undocumented values.
- Keep unknown or internal features read-only until their behavior is understood.
- Do not perform live flash-write tests on G305 or Superlight 2 hardware.
- Existing live profile-write proof is limited to the original Superlight path documented by the project.
- On macOS, keep validation read-only or limited to reversible live settings; do not test onboard flash writes.
- If the selected device, transport, or capability report is ambiguous, stop before issuing a mutation.

## Related documentation

- [Hardware feature matrix](../../docs/feature-matrix.md)
- [macOS testing guide](../../docs/macos-testing.md)
- [User-facing CLI](../cli/README.md)
- [Agent service](../agent/README.md)
