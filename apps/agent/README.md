# Open Hub agent

`open-hub-agent` is the required background service for Open Hub. It discovers supported mice, owns the HID sessions, persists settings, and exposes the local IPC API used by the CLI, tray, and future settings app.

Only the agent should communicate with a device during normal Open Hub use. User-facing clients should send requests over IPC instead of opening HID interfaces directly.

## Responsibilities

- Discover supported USB and receiver-connected devices.
- Maintain one managed session per physical device.
- Read capabilities and current settings from hardware.
- Validate and apply settings requested by IPC clients.
- Persist per-device settings by stable hardware ID.
- Publish device and settings events to subscribed clients.
- Release lighting control and device sessions during graceful shutdown.

## Build and run

From the workspace root:

```text
cargo build --release -p open-hub-agent
cargo run -p open-hub-agent
```

Run one discovery pass and exit:

```text
cargo run -p open-hub-agent -- --once
```

Use a specific settings file while developing or testing:

```text
cargo run -p open-hub-agent -- --settings-file ./open-hub-settings.json
```

See every option with:

```text
cargo run -q -p open-hub-agent -- --help
```

## Installation and per-user startup

The standalone setup bootstrapper installs and maintains the agent. The current
manifest default also selects the optional tray:

```text
open-hub-setup install
```

Install only the required headless service, or add the optional CLI:

```text
open-hub-setup install --components agent
open-hub-setup modify --components agent,tray,cli
```

The setup path is per-user and does not require administrator privileges. It verifies
the complete release bundle before stopping or replacing the agent. See the
[installation guide](../../docs/installation.md) for status, repair, and removal.

## IPC diagnostics

These modes turn the agent binary into a small diagnostic IPC client. Start a normal agent instance first.

Send one protocol request:

```powershell
'{"id":1,"protocol_version":1,"command":"ping"}' | cargo run -q -p open-hub-agent -- --request-stdin
```

Print events until interrupted, or stop after a fixed number:

```text
cargo run -q -p open-hub-agent -- --events
cargo run -q -p open-hub-agent -- --events --event-count 1
```

The user-facing [`open-hub` CLI](../cli/README.md) is the preferred interface for ordinary inspection and configuration.

## Settings and persistence

The agent owns the per-user settings document. Each device record is keyed by a stable hardware ID rather than by its temporary discovery index. Writes are atomic so an interrupted update cannot leave a partially written JSON file.

On startup, the agent loads persisted settings, discovers devices, and applies compatible values. Unsupported or invalid values are rejected instead of being forwarded blindly to hardware.

`--once` is intended for discovery diagnostics. It does not run the long-lived IPC service or perform the normal settings-management lifecycle.

## Runtime boundaries

- Do not run a normal agent and `open-hub-probe` against the same mouse at the same time.
- Fully exit Logitech G HUB before testing direct device access.
- The IPC endpoint is local to the current machine, but it is not an authentication boundary for untrusted local processes.
- Clients should tolerate device removal and reconnection; discovery indexes are not stable identifiers.

## Related documentation

- [IPC protocol](../../docs/ipc-protocol.md)
- [User-facing CLI](../cli/README.md)
- [Tray application](../tray/README.md)
- [Hardware feature matrix](../../docs/feature-matrix.md)
- [Installation](../../docs/installation.md)
