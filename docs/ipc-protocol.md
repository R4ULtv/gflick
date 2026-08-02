# Agent IPC protocol

Protocol version 1 uses newline-delimited JSON over an OS-local socket. Windows uses
the `open-hub-agent-v1.sock` named-pipe namespace. macOS uses the same filename under
the process's per-user temporary directory. This is local IPC rather than a network
listener.

The canonical Rust types are in `crates/open-hub-protocol`. A settings application
should depend on that crate when possible instead of duplicating the JSON schema.

## Message flow

Every request contains a caller-selected numeric `id`, `protocol_version`, and a
tagged `command`. Every normal response repeats the ID and protocol version. One JSON
value is written per line, with a 64 KiB maximum message size.

```json
{"id":1,"protocol_version":1,"command":"ping"}
```

```json
{"message":"response","id":1,"protocol_version":1,"result":{"status":"success","data":{"type":"pong"}}}
```

Supported read operations are `ping`, `list_devices`, and `get_device`. A device
snapshot includes identity, readiness, advertised capabilities, battery, DPI, polling,
configuration source, operating/surface modes, and BHOP.

Supported live-setting operations are:

- `set_dpi`
- `set_polling_rate`
- `set_lift_off_distance`
- `set_surface_mode`
- `set_operating_mode`
- `set_bunny_hopping`
- `set_lighting`
- `use_firmware_lighting`
- `use_host_settings`
- `use_onboard_profile`

Successful setting commands return a fresh complete device snapshot. The agent also
broadcasts a `settings_changed` event so other open clients can refresh immediately.
All validation and HID++ read-back checks remain inside `open-hub-core`.

## Events

A connection that sends `subscribe` becomes an event-only stream after receiving its
`subscribed` response. The event types are:

- `device_connected`: HID discovery found an interface; it may not yet be awake
- `device_ready`: the agent opened the mouse and read its initial state
- `device_unavailable`: three consecutive HID failures discarded a stale session;
  the next discovery pass will try to reopen it
- `device_disconnected`
- `battery_changed`
- `settings_changed`

For manual inspection:

```powershell
cargo run -q -p open-hub-agent -- --events
```

Use `--event-count 1` to exit after one event during automated diagnostics.

## Safety boundaries

The agent is the only process that owns open mouse sessions. Client threads submit
commands to that owner thread, preventing concurrent HID operations from racing.
Malformed JSON, unsupported protocol versions, missing devices, sleeping devices, and
HID operation failures have distinct structured error codes.

Protocol v1 does not expose onboard-flash editing. Profile reads and the existing
transactional flash API remain available through the diagnostic probe until the UI
workflow has explicit confirmation and recovery design. The local endpoint is not yet
an authentication boundary; production Windows service packaging must apply a
per-user named-pipe ACL.

On graceful Ctrl+C shutdown, the agent returns LED ownership to firmware for every
mouse whose lighting it controlled. A crash or forced process termination cannot run
that cleanup, so the eventual OS service wrapper must prefer graceful stop signals.
