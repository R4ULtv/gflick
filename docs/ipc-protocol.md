# Agent IPC protocol

Protocol version 2 uses newline-delimited JSON over an OS-local socket. Windows uses
the `gflick-agent-v2.sock` named-pipe namespace. macOS uses the same filename under
the process's per-user temporary directory. This is local IPC rather than a network
listener. Version 2 is not compatible with version 1: clients and agents must move
together because the socket namespace and externally tagged event enum changed.

The canonical Rust types are in `crates/gflick-protocol`. Rust clients should depend
on that crate when possible instead of duplicating the JSON schema.

## Message flow

Every request contains a caller-selected numeric `id`, `protocol_version`, and a
tagged `command`. Every normal response repeats the ID and protocol version. One JSON
value is written per line, with a 64 KiB maximum message size.

```json
{"id":1,"protocol_version":2,"command":"ping"}
```

```json
{"message":"response","id":1,"protocol_version":2,"result":{"status":"success","data":{"type":"pong"}}}
```

Supported read operations are `ping`, `list_devices`, `list_saved_devices`,
`get_device`, and `get_app_preferences`. `list_devices` reports currently discovered
interfaces; `list_saved_devices` reports every device retained in the agent's settings
file, including disconnected devices. A complete device snapshot includes identity,
readiness, advertised capabilities, battery, DPI, polling, configuration source,
operating/surface modes, BHOP, the host mouse-button mapping, and the current onboard
DPI stage when an onboard profile is active.

The lifecycle operation `shutdown` acknowledges an application-wide graceful shutdown
and then broadcasts `application_shutting_down`. The tray and Settings app close on
that event. This lets the separate components behave as one application without
coupling UI code to the HID process.

Supported live-setting operations are:

- `set_dpi` (same value on both axes)
- `set_dpi_axes` (`x`, `y`; independent axes where advertised)
- `set_mouse_button_mapping` (`mapping`; host control only, one 0–16 value per physical button)
- `set_onboard_dpi_stage` (`index` 0–4; active onboard profile required, no flash write)
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

Three further commands manage host-side presentation only:

- `set_device_color` stores one of the model's supported enclosure finishes. The color
  is cosmetic and is never sent as a lighting or firmware command.

- `set_device_nickname` stores a user-assigned display name, or clears it when
  `nickname` is null or blank. Names are trimmed and limited to 64 characters.
- `reorder_devices` accepts `hardware_ids` as the complete ordered prefix of
  saved devices. Omitted saved devices have their positions cleared and sort
  afterward. Duplicate or unknown hardware IDs are rejected, and an empty list
  clears the order.

None sends anything to the mouse. They therefore return `acknowledged` rather
than a device snapshot, emit no `settings_changed` event, and are exempt from the
device-ready guard so learned devices can be edited while offline or initializing.
Color selection is restricted to the finishes catalogued for the detected model.
After a successful durable save, the agent broadcasts exactly one
`device_metadata_changed` event containing the complete ordered `DeviceSummary`
list. Validation or persistence failures broadcast nothing.
Because all three persist under `hardware_id`, a device that has never been learned on
this host cannot have its metadata changed while offline. A previously learned offline
device may be edited when its USB identity resolves uniquely to its stored hardware
identity; unknown or ambiguous USB matches remain unavailable for metadata changes.

`get_app_preferences` and `set_app_preferences` read and replace the confirmation,
first-run, and sidebar-layout preferences shared by Settings windows. A successful set
returns `acknowledged`. It does not emit an event, so a client that changes these values
owns the immediate local UI update.

`list_devices` returns devices in stored order: those with a `sort_order` first, in
ascending order, followed by the rest in discovery order.

Each device summary contains two different identities:

- `id` is the current session's routing key and is used in IPC commands. When USB does
  not expose a serial number it can contain a hash of the HID path.
- `hardware_id` is an optional opaque physical-device key derived from the HID++ unit
  ID. It is normally learned at `device_ready` and is stable across ports, receiver
  paths, and wired/wireless transport. Persisted preferences must use this value and
  must never fall back to the routing ID.

`hardware_id` is optional because an unopened mouse may not have one yet. Recorded
messages from before the field existed still deserialize. A connected but not-yet-ready
mouse reports its stored hardware identity when a unique prior USB identity is known;
otherwise it remains absent until the agent can query the live device.

`display_name` is a separate additive optional field containing the model name reported
by the mouse through HID++. Some firmware does not expose it, particularly when connected
through a receiver. Clients must then fall back to the USB `product_name` and must not
guess a model from a receiver product ID.

Reading that name requires an open HID++ session, so a sleeping or disconnected mouse
cannot report it. The agent caches the last name each `hardware_id` reported and serves
it when no session is open, which stops a wireless mouse from appearing under its
receiver's USB product name while it is away. A live name always takes precedence over
the cached one.

The cached name stays separate from `nickname` in storage. A nickname is the user's
override and the cached name is the hardware's own identity, so keeping both lets a
renamed device still report which model it actually is.

A mouse that is asleep when the agent starts has never been opened, so its `hardware_id`
is unknown and stored preferences cannot be keyed by it. The agent therefore also records
the pre-HID++ USB identity — vendor, product, paired device index, and serial number —
each time a device is ready, and matches an unopened interface against it. That restores
the name, nickname, and list position immediately at startup. The paired device index is
part of the match because one receiver serves several mice. Empty and whitespace-only
USB serials are normalized to an absent serial, including values loaded from existing
version-1 settings. The agent first requires exactly one full identity match. If there is
no exact match and either side lacks a serial, it may use the vendor, product, and paired
device index only when exactly one stored device qualifies. Ambiguous matches fail closed,
so no cached name, nickname, list position, or `hardware_id` is attached to the unopened
interface. Two different nonblank serials never match through this fallback. A device that
has never been ready on this host still has no `display_name` until it wakes once.

`nickname`, `color`, and `sort_order` hold the host-side presentation metadata described
above. They are stored by the agent and never written to the device. `nickname` and
`sort_order` are optional; `color` defaults to `black` for older stored records and
protocol messages. Clients that show a nickname should keep the reported model name
visible somewhere, so the underlying hardware stays identifiable.

`DeviceSummary` also carries an `availability` object alongside the original `ready`
boolean. Its states are:

- `initializing`: the USB interface was just discovered and the first HID++ open is pending
- `ready`: the mouse is open and its complete settings snapshot is available
- `unavailable`: USB is still present, but the mouse is not responding or a later HID
  operation failed; the object includes a structured reason and diagnostic detail

An unavailable mouse may be asleep, switched off, or temporarily recovering from USB
enumeration. The agent retries it on every discovery pass without repeating identical
events. A successful retry emits `device_ready`; physical removal emits
`device_disconnected`.

## Preference persistence

The agent, not IPC clients, owns the per-user settings file. The file contains an `app`
section for shared application preferences and a `devices` section keyed by hardware
identity. Updating either section preserves the other. The first successful normal
discovery captures a baseline for a previously unknown `hardware_id`. After that, every
successful hardware-setting command updates the file only after HID++ read-back succeeds.
Battery and connection events never cause disk writes.

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

A connection that sends `subscribe` or `subscribe_settings` becomes an event-only
stream after receiving its `subscribed` response. `subscribe_settings` requests the
foreground five-second battery cadence for as long as that subscription remains alive;
ordinary tray subscriptions keep the configured background cadence. The event types are:

- `device_connected`: HID discovery found an interface; it may not yet be awake. When a
  unique prior USB identity is known, its stored hardware ID, nickname, sort position,
  and cached model name are already present in the summary.
- `device_ready`: the agent opened the mouse and read its initial state
- `device_unavailable`: an initial open failed, an asynchronous HID++ notification
  reported that a wireless link was lost while its USB receiver remained present, or
  repeated HID failures discarded a stale session; `reason_code` distinguishes
  `not_responding` from `communication_error`, and the next discovery pass retries it
- `device_disconnected`
- `battery_changed`
- `settings_changed`
- `device_metadata_changed`: authoritative host-side metadata for every currently
  discovered device, in the same order as `list_devices`. It includes availability,
  identity, names, and sort positions but deliberately excludes battery, DPI, and
  other live settings. Clients merge matching summaries with their existing live
  snapshots, add new summaries, and remove summaries absent from the event.
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

Protocol v2 does not expose onboard-flash editing. Profile reads and the existing
transactional flash API remain available through the diagnostic probe until the UI
workflow has explicit confirmation and recovery design. The local endpoint is not yet
an authentication boundary; production Windows service packaging must apply a
per-user named-pipe ACL.

On any graceful shutdown, including `shutdown`, Ctrl+C, or startup uninstallation, the
agent returns LED ownership to firmware for every mouse whose lighting it controlled.
A crash or forced process termination cannot run that cleanup, so application lifecycle
controls must always prefer the graceful IPC command or stop signal.
