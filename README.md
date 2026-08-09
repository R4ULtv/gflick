# GFlick

GFlick is an experimental Rust workspace for discovering and controlling compatible
Logitech mice through HID++.

The project is built around a reusable device library and a background agent. The
agent owns HID sessions and exposes a small local IPC protocol so a future desktop
client can read device state and apply validated settings without talking to HID
directly. Device support is capability-driven: a control is available only when the
mouse advertises it.

## Current status

GFlick is work in progress, not a finished Logitech G HUB replacement. The current
target is Windows 10/11 and Apple Silicon macOS. Intel macOS is outside the current
support and test matrix.

The agent and its protocol are the intended integration boundary. `gflick` is the
user-facing IPC CLI for controlling a running agent. `gflick-probe` remains a
development, diagnostic, and hardware-validation CLI that opens HID devices directly.

## Download and install

Release artifacts are currently **unsigned and experimental**. Download the user
bundle for Windows x86_64 or Apple Silicon macOS from the matching GitHub prerelease,
along with `SHA256SUMS`. Verify the archive before extracting it:

```powershell
# Windows PowerShell; compare this value with the archive's SHA256SUMS entry
(Get-FileHash .\gflick-0.1.1-windows-x86_64.zip -Algorithm SHA256).Hash.ToLower()
```

```sh
# Apple Silicon macOS
shasum -a 256 gflick-0.1.1-macos-aarch64.tar.gz
```

Extract the archive, then run `gflick-setup install` from the extracted directory.
Installing a release bundle does not require Rust or a compiler. See the
[setup README](apps/setup/README.md) for component selection, verification, repair,
and uninstall instructions.

The separately named `gflick-devtools-<version>-<platform>-<arch>` archive is only
for development and hardware diagnosis. Its Probe executable opens HID directly and
must not run at the same time as the agent.

## Early efficiency result

In an initial 30-minute Windows process-counter comparison, GFlick averaged `0.105%`
of one CPU core and `7.58 MiB` of resident memory. The complete resident G Hub stack
averaged `0.385%` of one core and `227.55 MiB`: GFlick used 72.8% less target CPU,
96.7% less resident memory, and more than 99.7% less recorded I/O. All 900 samples
matched without a target process restart.

This is a promising preliminary co-resident run, not a final electrical-power claim.
The exact environment, raw metrics, observer overhead, limitations, and reproducible
three-run procedure are documented in the [benchmark README](apps/bench/README.md).

## Workspace layout

| Path | Purpose |
| --- | --- |
| `crates/gflick-core` | HID/HID++ transport, discovery, capability models, settings, and onboard-profile APIs |
| `crates/gflick-client` | Typed synchronous client for requests and event subscriptions over local IPC |
| `crates/gflick-protocol` | Versioned serializable requests, responses, snapshots, and events for local IPC clients |
| [`apps/agent`](apps/agent/README.md) | `gflick-agent`, the device owner, monitor, settings store, and local IPC server |
| [`apps/cli`](apps/cli/README.md) | `gflick`, the user-facing scriptable IPC CLI for a running agent |
| [`apps/tray`](apps/tray/README.md) | `gflick-tray`, the native Windows notification-area and macOS menu-bar status client |
| [`apps/probe`](apps/probe/README.md) | `gflick-probe`, the developer-only direct-HID diagnostic and configuration CLI |
| [`apps/bench`](apps/bench/README.md) | `gflick-bench`, a development utility for recording and comparing resident-process resource usage |
| `docs` | Protocol, feature coverage, platform validation, and other project documentation |

In normal use, the data flow is:

```text
Logitech mouse -> HID/HID++ -> gflick-core -> gflick-agent -> gflick-client
                                      \-> gflick-probe          \-> gflick / gflick-tray
```

## What it can do

Depending on the connected mouse, the project can expose:

- device identity, firmware information, and battery state;
- DPI and polling-rate settings;
- lift-off distance, surface mode, operating mode, and BHOP settings;
- supported volatile lighting controls;
- onboard-profile discovery, decoding, selection, and selected profile edits through
  the core API and development CLI; and
- validated live writes with device read-back verification.

The exact feature set differs by model and firmware. See the [feature matrix](docs/feature-matrix.md)
for tested devices, current coverage, and intentional exclusions.

## Prerequisites

- Rust stable (the workspace currently declares Rust 1.85 as its minimum version);
- Windows: Visual Studio Build Tools with **Desktop development with C++** and a
  Windows SDK; or
- Apple Silicon macOS: Xcode Command Line Tools.

On macOS, HIDAPI shared-device access is enabled because a mouse can expose separate
short- and long-report collections.

## Build and check

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Run the agent

Run a single discovery pass and print the initial state:

```sh
cargo run -p gflick-agent -- --once
```

Run the background monitoring agent:

```sh
cargo run -p gflick-agent
```

The agent periodically discovers devices, keeps opened mouse sessions owned by one
thread, refreshes battery state, persists per-user preferences, and serves protocol
version 2 over an OS-local socket. Release bundles are installed and maintained by the
standalone setup bootstrapper:

```sh
gflick-setup install
gflick-setup install --components agent,tray,cli
```

The current default is `agent,tray`; use `--components agent` for a headless install.
Settings is the future primary UI and remains unavailable until its real payload ships.
See the [setup README](apps/setup/README.md) for bundle verification, component
selection, installed paths, status and repair, and data-retention behavior.

The [IPC protocol documentation](docs/ipc-protocol.md) covers message formats,
events, persistence, and safety boundaries.

See the [agent README](apps/agent/README.md) for startup installation, diagnostic
modes, persistence, and runtime boundaries.

## Run the tray status app

With the agent running, build and launch the native tray companion:

```sh
cargo build --release -p gflick-tray
./target/release/gflick-tray
```

It shows agent connectivity, current battery and DPI for every connected mouse. The
tray subscribes to IPC events and sleeps between changes; it does not access HID or poll
the agent continuously. See the [tray README](apps/tray/README.md) for platform behavior
and the initial Windows idle measurement.

## Inspect hardware with the probe

`gflick-probe` is useful when adding support or validating real hardware. It is a
developer-only direct-HID tool; use the IPC `gflick` CLI for normal user control
while the agent is running. Do not run Probe's direct-HID commands concurrently with
the agent, tray, or `gflick`, because they can compete for the same HID interface.

See the [probe README](apps/probe/README.md) for its complete command groups,
device-selection workflow, and hardware safety rules.

For normal user control with the agent running, see the [CLI documentation](apps/cli/README.md).
Use Probe for diagnostics and explicit dangerous profile-flash work:

```sh
cargo run -p gflick-probe -- list
cargo run -p gflick-probe -- devices
cargo run -p gflick-probe -- --help
```

The read-only commands include `list`, `devices`, `probe`, `features`, and `profiles`.
Explicitly named setting and profile commands can change device state. Onboard flash
writes require an explicit `--confirm-flash-write` acknowledgement and should only be
used after reading the relevant project documentation.

When testing real hardware, fully exit Logitech G HUB first; it can compete for HID++
interfaces and reapply its own active profile.

## Safety and scope

GFlick communicates with real device firmware. Unsupported inputs are rejected,
setting commands verify read-back, and profile writes preserve unknown data, validate
CRCs, and have rollback paths. Hardware validation is still device-specific; an
encoder test or a successful read on one model does not mean that every model is safe
to write.

The project intentionally does not provide host keyboard shortcuts/macros, onboard
macro creation, or per-application profiles. The local IPC endpoint is not intended to
be a general network service or a complete authentication boundary yet.

## Documentation

Detailed and component-specific information lives with each app and in [`docs/`](docs/):

- [Feature matrix](docs/feature-matrix.md) - supported capabilities, tested mice, and
  excluded HID++ features.
- [IPC protocol](docs/ipc-protocol.md) - protocol v1 messages, events, persistence,
  and client safety rules.
- [macOS testing](docs/macos-testing.md) - Apple Silicon build, discovery, and
  reversible hardware-validation checklist.
- [Setup and installation](apps/setup/README.md) - verified release bundles, selectable
  components, platform paths, repair, and uninstall retention.
- [Release process](docs/releasing.md) - maintainer versioning, packaging, publishing,
  validation, and rollback procedure.

The native benchmark's complete usage and A/B procedure live with the crate in
[apps/bench/README.md](apps/bench/README.md).

## License

GFlick is available under the [MIT License](LICENSE).
