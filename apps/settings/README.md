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

Use Refresh after connecting a mouse or starting the agent. No demo mode or 3D
renderer is included. External changes and battery updates are retrieved with
Refresh; a live event subscription is not yet implemented.

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

## Interface and assets

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
