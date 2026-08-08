# Agent IPC protocol

Protocol version 1 uses newline-delimited JSON over an OS-local socket. Windows uses
the `gflick-agent-v1.sock` named-pipe namespace. macOS uses the same filename under
the process's per-user temporary directory. This is local IPC rather than a network
listener.

The canonical Rust types are in `crates/gflick-protocol`. A settings application
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

The lifecycle operation `shutdown` acknowledges an application-wide graceful shutdown
and then broadcasts `application_shutting_down`. The tray and any future settings UI
must close on that event. This lets the separate components behave as one application
without coupling UI code to the HID process.

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
All validation and HID++ read-back checks remain inside `gflick-core`.

Two further commands manage host-side presentation only:

- `set_device_nickname` stores a user-assigned display name, or clears it when
  `nickname` is null or blank. Names are trimmed and limited to 64 characters.
- `reorder_devices` stores the device list order as a sequence of `hardware_id`
  values.

Neither sends anything to the mouse. They therefore return `acknowledged` rather
than a device snapshot, emit no `settings_changed` event, and are exempt from the
device-ready guard so a device can be renamed while it is still initializing.
Because both persist under `hardware_id`, they are unavailable until the agent has
opened the device and learned its hardware identity.

`list_devices` returns devices in stored order: those with a `sort_order` first, in
ascending order, followed by the rest in discovery order.

Each device summary contains two different identities:

- `id` is the current session's routing key and is used in IPC commands. When USB does
  not expose a serial number it can contain a hash of the HID path.
- `hardware_id` is an optional opaque physical-device key derived from the HID++ unit
  ID. It becomes available after `device_ready` and is stable across ports, receiver
  paths, and wired/wireless transport. Persisted preferences must use this value and
  must never fall back to the routing ID.

`hardware_id` is additive and optional in protocol v1 so older recorded messages without
it still deserialize. A connected but not-yet-ready mouse reports no hardware identity
until the agent can query it.

`display_name` is a separate additive optional field containing the model name reported
by the mouse through HID++. Some firmware does not expose it, particularly when connected
through a receiver. Clients must then fall back to the USB `product_name` and must not
guess a model from a receiver product ID.

`nickname` and `sort_order` are additive optional fields holding the host-side name
and list position described above. They are stored by the agent, never written to the
device, and are absent until the mouse has a `hardware_id`. Clients that show a
nickname should keep the reported model name visible somewhere, so the underlying
hardware stays identifiable.

`DeviceSummary` also carries an additive `availability` object while retaining the
original `ready` boolean for protocol-v1 clients. Its states are:

- `initializing`: the USB interface was just discovered and the first HID++ open is pending
- `ready`: the mouse is open and its complete settings snapshot is available
- `unavailable`: USB is still present, but the mouse is not responding or a later HID
  operation failed; the object includes a structured reason and diagnostic detail

An unavailable mouse may be asleep, switched off, or temporarily recovering from USB
enumeration. The agent retries it on every discovery pass without repeating identical
events. A successful retry emits `device_ready`; physical removal emits
`device_disconnected`.

## Preference persistence

The agent, not IPC clients, owns the per-user settings file. The first successful normal
discovery captures a baseline for a previously unknown `hardware_id`. After that, every
successful setting command updates the file only after HID++ read-back succeeds. Battery
and connection events never cause disk writes.

Host/local preferences are reapplied when a ready mouse reconnects. For extended polling,
only the value matching the current wired or wireless transport is written. Selecting an
onboard profile stores its sector number without copying or rewriting profile flash, and
the previously saved host preferences remain available if the user later switches back.
The diagnostic `--once` mode does not apply or capture preferences.

If a device write succeeds but the atomic settings-file replacement fails, the response
uses `persistence_failed` and explicitly reports that the live setting changed but was
not saved. This lets a settings client refresh the device without falsely claiming the
preference is durable.

## Events

A connection that sends `subscribe` becomes an event-only stream after receiving its
`subscribed` response. The event types are:

- `device_connected`: HID discovery found an interface; it may not yet be awake
- `device_ready`: the agent opened the mouse and read its initial state
- `device_unavailable`: an initial open failed, an asynchronous HID++ notification
  reported that a wireless link was lost while its USB receiver remained present, or
  repeated HID failures discarded a stale session; `reason_code` distinguishes
  `not_responding` from `communication_error`, and the next discovery pass retries it
- `device_disconnected`
- `battery_changed`
- `settings_changed`
- `application_shutting_down`: the agent is exiting and all subscribed UI components
  should close without attempting to reconnect

For manual inspection:

```powershell
cargo run -q -p gflick-agent -- --events
```

Use `--event-count 1` to exit after one event during automated diagnostics.

### Disconnecting

A client may hang up at any time without sending anything first, and is expected to
reconnect freely — the settings window retries every five seconds.

The agent watches each subscriber's read half and drops that subscriber as soon as it
reports end-of-stream, rather than waiting for the next published event to fail
writing. Otherwise a client leaving during a quiet period holds its slot and parks a
thread until an event happens to arrive, which on an idle mouse can be a long time.

Nothing is written to a connection to test it: clients parse every line, so a probe
byte — even a bare newline — would be a parse error on a healthy connection.

Errors that only mean the peer left (`BrokenPipe`, `ConnectionReset`,
`ConnectionAborted`, `UnexpectedEof`, and Windows `ERROR_NO_DATA`/232) are not logged.
Any other failure on a client connection still prints.

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

On any graceful shutdown, including `shutdown`, Ctrl+C, or startup uninstallation, the
agent returns LED ownership to firmware for every mouse whose lighting it controlled.
A crash or forced process termination cannot run that cleanup, so application lifecycle
controls must always prefer the graceful IPC command or stop signal.
