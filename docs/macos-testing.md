# macOS validation

GFlick supports Windows 10/11 and macOS on Apple Silicon from the same Rust core.
Intel macOS is intentionally unsupported and is not part of this checklist. Run it on
real Apple Silicon hardware after installing the Xcode Command Line Tools and Rust
stable. No settings are changed by the commands in the first three sections.

## Validated baseline

The complete checklist passed on 2026-08-02 using a Mac16,12 with an Apple M4, macOS
15.7.7, and Rust 1.97.1. Debug and release workspace builds completed without warnings;
all 38 tests, formatting, and strict all-target Clippy passed.

The original PRO X Superlight, G305, and PRO X Superlight 2 all passed discovery,
feature audits, profile decoding with valid CRCs, agent snapshots, IPC requests, and
reversible live-setting tests. Direct-USB disconnect/connect/ready events were verified.
No onboard flash was written, every live setting was restored, firmware lighting control
was returned to the G305, and graceful shutdown left no stale local socket.

Subsequent tray validation on the same Apple Silicon system covered the installed
application bundle, automatic per-user startup, native template icon, battery/DPI title,
event-driven settings updates, wireless unavailable/recovery transitions with the
receiver still connected, and application-wide quit. The agent and tray then remained
stable for all 900 samples of a 30-minute idle benchmark. See the
[benchmark report](../apps/bench/README.md#initial-apple-silicon-macos-result) for the
resource measurements and test limitations.

A 61-second release-agent sample using deliberately aggressive 2-second discovery and
30-second battery intervals averaged 0.28% CPU and 3,847 KiB RSS. This is a development
baseline, not a final power-efficiency claim.

Sleeping receiver-connected mice occasionally timed out during their initial root-feature
query and worked after being woken. One GPX2 profile read encountered an IOKit SetReport
timeout and succeeded immediately with all five CRCs valid in a fresh session. A changed
extended-DPI request made while the GPX2 was under onboard control returned HID++ 2.0
`NOT_ALLOWED` (`0x05`); the same request succeeded in host mode and the original onboard
profile was restored. These observations did not justify a speculative transport change.

## 1. Build validation

```sh
rustc --version
cargo fmt --all -- --check
cargo build --workspace
cargo build --release --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Record the Apple Silicon model, architecture, and macOS version. The core, typed IPC
client, command-line applications, and native tray must build without platform-specific
source changes.

### Menu-bar validation

With the installed agent ready, launch the release tray:

```bash
./target/release/gflick-tray
```

Confirm that it runs as an accessory/menu-bar application without a Dock icon. Its title
and menu must show the current battery and DPI, update after a reversible DPI change,
represent agent shutdown/restart without stale values, and reconnect automatically.
Record a settled 60-second CPU, RSS, thread, and wake-up baseline. `Quit GFlick` must
gracefully stop both the tray and agent, and launchd must not restart either component
until the next login or explicit startup installation.

### Per-user setup and LaunchAgent validation

After the release build succeeds, validate installation separately from the hardware
write checks:

```bash
./gflick-setup verify-bundle
./gflick-setup install --components agent,tray
./gflick-setup status
launchctl print "gui/$(id -u)/io.github.r4ultv.gflick.agent"
launchctl print "gui/$(id -u)/io.github.r4ultv.gflick.tray"
```

Confirm that the agent is under `~/Library/Application Support/gflick/bin`, the tray
is installed as `~/Library/Application Support/gflick/GFlick.app`, and the plist is
under `~/Library/LaunchAgents`. Finder must recognize the bundle's full-color ICNS icon,
while the menu bar must render the transparent template cleanly in both light and dark
appearances. The independent agent and tray LaunchAgents must both start automatically,
and the agent
must become ready through its normal IPC endpoint. Log out and back in once to verify
automatic login startup. Then verify graceful
unregistration and reinstall it for continued testing:

```bash
./gflick-setup uninstall
./gflick-setup status
./gflick-setup install --components agent,tray
```

Inspect `~/Library/Logs/gflick/agent.log`, `agent-error.log`, `tray.log`, and
`tray-error.log` for launch-only failures. The uninstall status must report no managed
files or loaded LaunchAgents. It must preserve settings and logs unless
`--remove-user-data` is explicitly supplied.

## 2. HID discovery

Fully exit Logitech G HUB so it cannot compete for the HID++ interfaces, then run:

```sh
cargo run -p gflick-probe -- list
cargo run -p gflick-probe -- devices
cargo run -p gflick-agent -- --once
```

Confirm that each mouse appears once in `devices`, receiver and direct-USB connections
are distinguished correctly, and the agent can open each discovered stable ID.

## 3. Read-only feature proof

Use the VID:PID and HID++ index printed by discovery. Typical values for the three tested
devices are shown below:

```sh
cargo run -p gflick-probe -- probe --index 046d:c094 --device-index ff
cargo run -p gflick-probe -- profiles --index 046d:c094 --device-index ff

cargo run -p gflick-probe -- probe --index 046d:c53f --device-index 1
cargo run -p gflick-probe -- profiles --index 046d:c53f --device-index 1

cargo run -p gflick-probe -- probe --index 046d:c54d --device-index 1
cargo run -p gflick-probe -- profiles --index 046d:c54d --device-index 1
```

Compare device identity, battery, DPI, polling rates, configuration source, profile
count, and CRC status with the Windows results. A different numeric list index is
normal; stable identity must not depend on that index.

## 4. Reversible setting tests

Only run write tests after the read-only proof succeeds. Record every starting value,
change one setting, verify the read-back, and restore the original value immediately.
Start with live DPI because it does not touch onboard flash. Then test polling rate,
LOD, Surface Mode, and BHOP only where the mouse advertises them.

Do not run `prove-profile-write` on the Superlight 2. The only completed live profile
flash proof is disabled profile 5 on the old wired Superlight under Windows.

## 5. Report portability failures

Keep the complete command output for any failure. In particular, preserve the HID
interface path, interface number, usage page/usage, VID:PID, and whether G HUB was
running. Discovery or permission differences belong in a small macOS transport adapter;
HID++ feature parsing and mouse models must remain platform-neutral.
