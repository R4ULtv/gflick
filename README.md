# open-hub

An experimental Logitech HID++ core, background agent, and probe. It discovers HID
interfaces; reads identity, firmware, battery, DPI, polling rate, onboard profiles,
button mappings, lift-off distance, surface mode, and BHOP state; and provides
explicit validated commands for changing supported live settings.

## Workspace structure

- `crates/open-hub-core`: reusable HID++ transport, feature discovery, battery, DPI,
  wired/wireless polling-rate, and profile-control APIs
- `crates/open-hub-protocol`: versioned serializable IPC requests, responses, snapshots,
  and events shared by the agent and future settings clients
- `apps/agent`: low-overhead device owner, monitor, and local IPC server
- `apps/probe`: thin diagnostic CLI built on `open-hub-core`

The core models host/local settings separately from onboard-profile control. Switching
to host control does not erase the profiles stored in the mouse. Stored profile formats
`0x03`, `0x04`, and `0x07` support byte-preserving edits, CRC regeneration, transactional
writes, read-back verification, and automatic rollback.

## Project scope

Open Hub targets Windows 10/11 and macOS on Apple Silicon (`aarch64-apple-darwin`).
Intel macOS is intentionally outside the supported and tested platform matrix. The
shared Rust core, probe, and monitoring agent must behave consistently on the supported
platforms; platform-specific code will be isolated behind small adapters when service
installation or UI integration is added.

Host keyboard shortcuts/macros, onboard macro creation, and per-application profiles
are intentional non-goals. Button assignments stored directly in normal onboard
profiles remain in scope.

The service-facing API is `MouseDevice`. It discovers and caches supported HID++
features, exposes `capabilities()` and `settings()` snapshots, and owns validated DPI
and wired/wireless polling-rate writes with read-back verification:

```rust
let mouse = MouseDevice::discover(session)?;
let capabilities = mouse.capabilities()?;
let settings = mouse.settings()?;

mouse.set_dpi(1600)?;
mouse.set_polling_rate(ConnectionType::GamingWireless, 2000)?;
mouse.set_lift_off_distance(LiftOffDistance::High)?;
mouse.set_surface_mode(SurfaceMode::Automatic)?;
mouse.set_operating_mode(OperatingMode::Performance)?;
mouse.set_bunny_hopping(true, 100)?;
```

`DeviceManager` owns HID enumeration for the agent. It refreshes the HIDAPI
device list, discovers Logitech HID++ short-report collections, pairs their companion
long-report collections, assigns direct-USB or receiver device indexes, and opens a
ready `MouseDevice` by stable in-memory ID. `refresh_with_changes()` reports stable-ID
connection and disconnection differences between discovery passes.

The in-memory ID remains the routing key for commands during the current agent session.
Once a mouse is ready, the agent also exposes an opaque `hardware_id` derived from its
HID++ unit ID. Unlike the routing ID's USB-path fallback, the hardware ID is independent
of ports, receiver paths, and wired/wireless transport, so future persisted preferences
can follow the physical mouse safely.

Polling capabilities are represented explicitly as `Shared` for legacy `0x8060`
devices or `PerConnection { wired, wireless }` for extended `0x8061` devices. This
prevents older mice from appearing to have two independently configurable rates.

## Prerequisites

- Rust stable
- Windows: Visual Studio Build Tools with **Desktop development with C++** and a
  Windows SDK
- macOS on Apple Silicon: Xcode Command Line Tools

On macOS, Open Hub enables HIDAPI shared-device access because Logitech short- and
long-report collections can require separate handles to the same physical device.

## Build

```powershell
cargo build --workspace
```

The macOS hardware-validation checklist is in
[docs/macos-testing.md](docs/macos-testing.md).

## Run the agent

Run one discovery pass and exit (useful for testing):

```powershell
cargo run -p open-hub-agent -- --once
```

Run the monitoring loop:

```powershell
cargo run -p open-hub-agent
```

The agent scans USB discovery every 5 seconds, keeps successfully opened mouse handles
alive, and reads only battery state every 5 minutes. It exposes protocol v1 over an OS
local socket (a named pipe on Windows and a Unix-domain socket on macOS). HID access
remains serialized on the agent thread while client connections sleep independently.
The intervals can be adjusted with `--scan-interval-seconds` and
`--battery-interval-seconds`. Repeated HID failures discard and reopen a stale session,
and graceful shutdown returns any software-controlled lighting to device firmware.
State persistence remains intentionally deferred.

Send a diagnostic request to a running agent through standard input:

```powershell
'{"id":1,"protocol_version":1,"command":"ping"}' |
  cargo run -q -p open-hub-agent -- --request-stdin
```

Watch connection, readiness, battery, and settings-change events:

```powershell
cargo run -q -p open-hub-agent -- --events
```

The protocol schema, commands, event model, and safety boundaries are documented in
[docs/ipc-protocol.md](docs/ipc-protocol.md).

## Use the probe

First list the Logitech HID interfaces:

```powershell
cargo run -p open-hub-probe -- list
```

For normal automatic discovery, inspect every connected supported mouse without any
VID:PID or HID++ index arguments:

```powershell
cargo run -p open-hub-probe -- devices
```

Select an entry with a vendor-defined usage page (`0xff00` or higher), then run:

```powershell
cargo run -p open-hub-probe -- probe --index <INDEX>
```

You can also select a unique receiver directly by USB vendor and product ID:

```powershell
cargo run -p open-hub-probe -- probe --index 046d:c54d --device-index 1
```

The probe defaults to HID++ device index `1` for a receiver and `ff` for a directly
connected device. Override it when necessary:

```powershell
cargo run -p open-hub-probe -- probe --index <INDEX> --device-index 1
cargo run -p open-hub-probe -- probe --index <INDEX> --device-index ff
```

The `probe` command sends only HID++ read/query requests.

For protocol research, dump every advertised HID++ feature with:

```powershell
cargo run -p open-hub-probe -- features --index 046d:c54d --device-index 1
```

To set both sensor axes to an advertised DPI value and verify the result:

```powershell
cargo run -p open-hub-probe -- set-dpi --index 046d:c54d --device-index 1 --dpi 1600
```

The Superlight 2 also exposes lift-off distance, Gaming Surface Mode, and BHOP:

```powershell
cargo run -p open-hub-probe -- set-lift-off-distance --index 046d:c54d --device-index 1 --lod high
cargo run -p open-hub-probe -- set-surface-mode --index 046d:c54d --device-index 1 --mode auto
cargo run -p open-hub-probe -- set-bhop --index 046d:c54d --device-index 1 --state on --window-ms 100
```

LOD accepts `low`, `medium`, or `high`. Surface Mode accepts `on`, `auto`, or `off`.
BHOP accepts `on` or `off`; its timeout is 100–1000 ms in 100 ms steps.

To set the advertised wireless receiver polling rate and verify the result:

```powershell
cargo run -p open-hub-probe -- set-polling-rate --index 046d:c54d --device-index 1 --hz 2000
```

The command defaults to wireless read-back. Use `--connection wired` when the mouse is
connected directly by USB and you intentionally want to verify the wired setting.

If `probe` reports that an onboard profile is enabled, explicitly switch the mouse to
host control while setting the polling rate:

```powershell
cargo run -p open-hub-probe -- set-polling-rate --index 046d:c54d --device-index 1 --hz 2000 --disable-onboard-profiles
```

This mode switch does not erase the profiles stored on the mouse. Fully exit Logitech
G HUB during testing so it cannot reapply its own active profile.

Profile-control mode can also be changed explicitly and verified:

```powershell
cargo run -p open-hub-probe -- use-host-settings --index 046d:c094 --device-index ff
cargo run -p open-hub-probe -- use-onboard-profile --index 046d:c094 --device-index ff --profile 1
```

`use-onboard-profile` selects an existing profile; it does not rewrite its flash data.

For development only, an explicitly acknowledged command proves the complete onboard
flash-write path using a disabled profile. It refuses enabled profiles, writes an
identical CRC-valid image, and verifies every byte:

```powershell
cargo run -p open-hub-probe -- prove-profile-write --index 046d:c094 --device-index ff --profile 5 --confirm-flash-write
```

Normal profile saves use stale-data detection, preserve unknown sector bytes,
regenerate the HID++ CRC, verify the read-back, and automatically restore the backup
if verification fails.

Typed profile commands cover names, DPI stages/default/shift selection, per-connection
polling, power timeouts, format `0x07` BHOP, non-macro button assignments, and enabled
state. Every command requires explicit flash acknowledgement:

```powershell
cargo run -p open-hub-probe -- set-profile-name --index 046d:c094 --device-index ff --profile 5 --name Gaming --confirm-flash-write
cargo run -p open-hub-probe -- set-profile-dpi --index 046d:c094 --device-index ff --profile 5 --stage 1 --dpi 800 --make-default --confirm-flash-write
cargo run -p open-hub-probe -- set-profile-button --index 046d:c094 --device-index ff --profile 5 --button 4 --confirm-flash-write mouse --mouse-button 4
cargo run -p open-hub-probe -- set-profile-enabled --index 046d:c094 --device-index ff --profile 5 --state on --confirm-flash-write
```

Setting commands query the values advertised by the mouse, reject unsupported
inputs, perform the write, and read the value back. These are live settings; Logitech
G HUB or an enabled onboard profile may subsequently replace them.

For the PRO X Superlight 2 receiver (`046d:c54d`), the probe automatically pairs its
interface 2 short- and long-report collections. Use HID++ device index `1`. If opening
the interface or receiving replies fails, fully exit Logitech G HUB and retry.

The G305 receiver is `046d:c53f` with HID++ device index `1`. Its public LED zone can
be driven without touching onboard flash, then returned to normal firmware control:

```powershell
cargo run -p open-hub-probe -- set-lighting --index 046d:c53f --device-index 1 fixed --color 00ff00
cargo run -p open-hub-probe -- use-firmware-lighting --index 046d:c53f --device-index 1
```

## Current HID++ support

- Protocol version negotiation
- Root `GetFeature` discovery
- Named feature classification (user, diagnostic, maintenance, and internal)
- Device identity, model, serial number, and firmware entities (`0x0003`, `0x0005`)
- Unified Battery (`0x1004`)
- Battery Status fallback (`0x1000`)
- Extended Adjustable DPI (`0x2202`)
- Adjustable DPI fallback (`0x2201`)
- Extended Adjustable Report Rate (`0x8061`)
- Report Rate fallback (`0x8060`)
- Performance/endurance Mode Status (`0x8090`)
- Gaming Surface Mode through Mode Status (`0x8090`)
- Volatile Color LED Effects and firmware/software control (`0x8070`)
- Bunny Hopping (`0x80e0`)
- Onboard Profiles mode selection, layout discovery, decoding, and transactional writes (`0x8100`)
- Profile formats `0x03`, `0x04`, and BHOP-aware format `0x07`
- Stored DPI stages/LOD/BHOP, profile names, report rates, power timers, and button assignments read/write API
- Host mouse-button mapping read/write API (`0x8110`)
- Validated DPI, lift-off-distance, surface-mode, BHOP, and polling-rate writes with
  read-back verification

## Tested mice

- Logitech G Pro X Superlight (original), direct USB `046d:c094`, HID++ index `ff`:
  Battery `0x1004`, Adjustable DPI `0x2201`, Report Rate `0x8060`, and Onboard
  Profiles `0x8100`
- Logitech G Pro X Superlight 2, Lightspeed receiver `046d:c54d`, HID++ index `1`:
  Battery `0x1004`, Extended Adjustable DPI `0x2202`, Extended Report Rate `0x8061`,
  Mode Status `0x8090`, Bunny Hopping `0x80e0`, and Onboard Profiles `0x8100`
- Logitech G305 Lightspeed, receiver `046d:c53f`, HID++ index `1`: Battery `0x1000`,
  Adjustable DPI `0x2201`, Report Rate `0x8060`, performance/endurance Mode Status
  `0x8090`, Color LED Effects `0x8070`, and format `0x03` Onboard Profiles `0x8100`

All three devices have passed read-only snapshots, decoded profile reads, and CRC checks
on Windows and Apple Silicon macOS. On an Apple M4 running macOS 15.7.7, both debug and
release workspace builds, all 38 tests, formatting, and strict Clippy passed. Reversible
live DPI and polling changes passed on every mouse. The Superlight 2 additionally passed
LOD, Surface Mode, and BHOP round trips; the G305 passed operating-mode and all four
volatile lighting-effect round trips before firmware lighting control was restored.

The macOS release agent averaged 0.28% CPU and 3,847 KiB RSS during a 61-second diagnostic
run with deliberately aggressive 2-second discovery and 30-second battery intervals.
Sleeping wireless mice occasionally required a fresh-session retry, and one non-repeating
IOKit report timeout recovered immediately; neither required a platform-specific code
change. See [docs/macos-testing.md](docs/macos-testing.md) for the complete validation
scope and safety boundaries.

The two Superlight models have also passed Windows DPI/polling writes with read-back
verification. The Superlight 2 passed reversible LOD (`high -> medium -> high`), Surface
Mode (`off -> auto -> off`), and BHOP (`off -> 100 ms/on -> off`) round trips. G305
profile-format encoding is unit-tested and its onboard flash has not been changed. Its
four advertised volatile LED effects passed live read-back tests before firmware control
was restored. Its polling rate passed a host-control `1000 -> 500 -> 1000 Hz` round trip,
after which onboard profile `0x0001` was reactivated.

The original Superlight has passed a complete 255-byte identical-data flash write to
disabled profile 5, a temporary profile-name edit with exact 255-byte restoration, and
an enable/disable directory round trip. All read-backs and CRCs were valid. Profile
formats `0x03` and `0x07` are unit-tested, but no profile flash write has been sent to
the G305 or Superlight 2.

Run the complete named capability audit and decoded profile proof with:

```powershell
cargo run -p open-hub-probe -- features --index 046d:c54d --device-index 1
cargo run -p open-hub-probe -- profiles --index 046d:c54d --device-index 1
```

The full end-user coverage matrix and the intentionally excluded firmware endpoints
are tracked in [docs/feature-matrix.md](docs/feature-matrix.md).

The general `probe` command remains read-only. Only explicitly named `set-*` and
profile-control commands change device state.
