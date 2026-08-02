# macOS validation

Open Hub supports Windows 10/11 and macOS from the same Rust core. Run this checklist
on real macOS hardware after installing the Xcode Command Line Tools and Rust stable.
No settings are changed by the commands in the first three sections.

## 1. Build validation

```sh
rustc --version
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Record whether the Mac is Apple Silicon or Intel and the macOS version. Both the core
and the two command-line applications must build without platform-specific source
changes.

## 2. HID discovery

Fully exit Logitech G HUB so it cannot compete for the HID++ interfaces, then run:

```sh
cargo run -p open-hub-probe -- list
cargo run -p open-hub-probe -- devices
cargo run -p open-hub-agent -- --once
```

Confirm that each mouse appears once in `devices`, receiver and direct-USB connections
are distinguished correctly, and the agent can open each discovered stable ID.

## 3. Read-only feature proof

Use the VID:PID and HID++ index printed by discovery. Typical values for the two tested
devices are shown below:

```sh
cargo run -p open-hub-probe -- probe --index 046d:c094 --device-index ff
cargo run -p open-hub-probe -- profiles --index 046d:c094 --device-index ff

cargo run -p open-hub-probe -- probe --index 046d:c54d --device-index 1
cargo run -p open-hub-probe -- profiles --index 046d:c54d --device-index 1
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
