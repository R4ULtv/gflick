# GFlick Settings (GPUI)

The native Rust settings client uses [GPUI Kit](https://gpui-kit.com), with a
layout inspired by `design/settings-studio`. All device reads and writes go
through `gflick-client`; the agent owns hardware access and persistence.

## Run

Start the agent, then launch the settings window in another terminal:

```sh
cargo run -p gflick-agent
cargo run -p gflick-settings
```

Battery percentage, charging status, mouse availability, and USB connection
changes update automatically through the same agent event stream used by the tray.
The client reconnects when the agent restarts. The agent defaults to checking USB
connections every 5 seconds. Battery/charging status is checked every 5 seconds
while any Settings window is connected, returning to 30 seconds when the last
window closes or crashes. A tray-only subscription uses the background interval.
`--battery-interval-seconds` changes that background interval (for example, 60);
`--scan-interval-seconds` controls discovery. No additional battery polling runs
in the client. Restart both the agent and Settings to use adaptive polling.

Use Refresh for external changes to DPI, polling, and other editable settings.
A newly connected mouse gets an initial settings snapshot. Battery-only updates
preserve drafts and input focus. No demo mode or 3D renderer is included.

## Editing

Choose values in Performance or Device details, then select **Apply**. **Discard** restores the last
read values. Drafts are independent for each device and survive switching devices.
Refresh is disabled while that device has pending edits. Controls are disabled
while a write is in progress.

Only settings advertised by the selected mouse are offered:

- DPI entry and presets, validated against the actual supported increments;
- shared polling or independent wired/wireless rates, as reported by the mouse;
- lift-off distance, surface mode, operating mode, and bunny hopping with timeout;
- host control and selection of existing onboard profiles;
- lighting zone, off/fixed/cycling/breathing effects, color, period and brightness,
  and returning control to firmware;
- a saved device nickname in Device details, with an empty name restoring the reported name;
- model-specific enclosure colors in Device details: Superlight 1 (white, black, red, magenta), Superlight 2 (white, black, cyan, magenta), and G305 (white, black, lilac, blue, mint).

DPI writes apply to both axes: the current agent API accepts one DPI value.
Polling changes from onboard control require explicitly selecting **Host control**
before Apply. Selecting an onboard profile must be applied separately from sensor
edits, so those edits start from that profile's actual values. Stored profile
contents, DPI stages, button assignments and macros are not exposed by the current
agent protocol and are not edited here. Lighting state is not included in device
snapshots; the form identifies it as unreported and starts with **Unchanged**.

Before writing, the client re-reads the device and checks for external setting or
hardware-identity changes. Commands run in order on a background executor, stop at
the first error, and end with a fresh device read. A batch is not atomic: completed
writes may remain after an error. The form shows the read-back and retains failed
or unattempted edits for review and retry. A failed final read is reported as an
error, never as a successful apply.

## App preferences

Open **App preferences** in the sidebar. Changes save immediately through the
agent into the `app` section of `gflick/settings.json`, alongside `devices`.
The agent is the sole writer, so a client preference update preserves device
settings and a hardware update preserves app preferences. The agent must be
running to read or change preferences.

- **Launch at sign in** controls the agent's macOS LaunchAgent or Windows login
  entry and reflects the existing system setting.
- **Show tray icon** starts or stops GFlick Tray independently of the agent and
  enables or disables the tray’s own login entry. It reflects the existing tray
  configuration. First-run setup enables both login items once; later launches
  preserve the user’s choice. Closing Settings does not start the tray. Build or
  install `gflick-tray` alongside the settings binary to enable it.
- **Confirm Apply** and **Confirm profile writes** default to on; **Confirm Discard**
  defaults to off. Profile confirmation is marked recommended. Enabled confirmations show the device and pending changes; Cancel
  leaves the draft intact. Enter and Escape cancel; use the explicit Apply or
  Discard button to proceed. The profile option asks when applying hardware changes
  under onboard control or selecting an onboard profile; stored profile contents
  are not edited by this client.
- **Agent** shows the local connection status and offers Restart. Restart is
  disabled while device changes are pending or being written. A standalone agent
  must be installed or built alongside the settings binary; an installed macOS
  LaunchAgent is restarted through launchctl.

## Interface and assets

- Previously seen mice remain in the sidebar, including after a restart. Identity
  identities come from the agent’s `settings.json` device entries, without a
  separate client history file.
  “Mouse offline · receiver connected” distinguishes a silent wireless mouse
  from “USB disconnected.” Connection changes update automatically; loading
  or a lost agent connection keeps rows visible with unverified status and no stale battery.
- Sidebar battery percentage and low-charge indicator; unknown charge shows a dash.
- Performance and Device details tabs, with a stacked layout at narrower widths.
- Website logo rendered at display resolution and package-derived app version.
- Product previews appear only in Device details.
- Official transparent top-view artwork for every supported finish, matched to reported model names,
  independently of nicknames. Unsupported models do not borrow another photo.
- Photos are prefiltered at build time with alpha-aware Lanczos filtering and
  embedded at 1×/2× densities. Original source assets are not edited at runtime.

Color choices update the preview immediately. Apply saves `color` in the agent’s
`settings.json` entry keyed by physical hardware ID; Discard restores the saved
image. Both the UI and agent validate colors against the selected model. Existing files without a color default to black. Color is cosmetic host
metadata and never changes mouse firmware or lighting. Rebuild and restart both
the agent and settings client to use the new `set_device_color` command.

The native client requires Rust 1.97.1, as declared in its manifest; the rest of the
workspace retains its Rust 1.85 minimum.

## Validation

```sh
cargo test -p gflick-settings
cargo clippy -p gflick-settings --all-targets
```

Tests cover capability validation, explicit host control, profile switching,
partial batch failures, stale-state protection, nickname acknowledgements,
read-back failures, preserving drafts and transparent image edges. The write-flow
tests use a simulated agent and do not change connected hardware.

Image and swatch provenance, including the model-specific sRGB values, is in
[`assets/mouses/SOURCES.md`](../../assets/mouses/SOURCES.md).
