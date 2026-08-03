# Open Hub

Open Hub is an experimental Rust workspace for discovering and controlling compatible
Logitech mice through HID++.

The project is built around a reusable device library and a background agent. The
agent owns HID sessions and exposes a small local IPC protocol so a future desktop
client can read device state and apply validated settings without talking to HID
directly. Device support is capability-driven: a control is available only when the
mouse advertises it.

## Current status

Open Hub is work in progress, not a finished Logitech G HUB replacement. The current
target is Windows 10/11 and Apple Silicon macOS. Intel macOS is outside the current
support and test matrix.

The agent and its protocol are the intended integration boundary. `open-hub-probe` is
currently a development, diagnostic, and hardware-validation CLI. Its command surface
may change as it is expanded into a more complete user-facing CLI or consolidated with
another tool.

## Workspace layout

| Path | Purpose |
| --- | --- |
| `crates/open-hub-core` | HID/HID++ transport, discovery, capability models, settings, and onboard-profile APIs |
| `crates/open-hub-protocol` | Versioned serializable requests, responses, snapshots, and events for local IPC clients |
| `apps/agent` | `open-hub-agent`, the device owner, monitor, settings store, and local IPC server |
| `apps/probe` | `open-hub-probe`, the experimental diagnostic and configuration CLI |
| `apps/bench` | `open-hub-bench`, a development utility for recording and comparing resident-process resource usage |
| `docs` | Protocol, feature coverage, platform validation, and other project documentation |

In normal use, the data flow is:

```text
Logitech mouse -> HID/HID++ -> open-hub-core -> open-hub-agent -> local client
                                      \-> open-hub-probe (development tool)
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
cargo run -p open-hub-agent -- --once
```

Run the background monitoring agent:

```sh
cargo run -p open-hub-agent
```

The agent periodically discovers devices, keeps opened mouse sessions owned by one
thread, refreshes battery state, persists per-user preferences, and serves protocol
version 1 over an OS-local socket. It can also be installed for per-user startup with
the `startup` subcommands. Run `cargo run -p open-hub-agent -- --help` for all modes.

The [IPC protocol documentation](docs/ipc-protocol.md) covers message formats,
events, persistence, and safety boundaries.

## Inspect hardware with the probe

The probe is useful when adding support or validating a real device:

```sh
cargo run -p open-hub-probe -- list
cargo run -p open-hub-probe -- devices
cargo run -p open-hub-probe -- --help
```

The read-only commands include `list`, `devices`, `probe`, `features`, and `profiles`.
Explicitly named setting and profile commands can change device state. Onboard flash
writes require an explicit `--confirm-flash-write` acknowledgement and should only be
used after reading the relevant project documentation.

When testing real hardware, fully exit Logitech G HUB first; it can compete for HID++
interfaces and reapply its own active profile.

## Safety and scope

Open Hub communicates with real device firmware. Unsupported inputs are rejected,
setting commands verify read-back, and profile writes preserve unknown data, validate
CRCs, and have rollback paths. Hardware validation is still device-specific; an
encoder test or a successful read on one model does not mean that every model is safe
to write.

The project intentionally does not provide host keyboard shortcuts/macros, onboard
macro creation, or per-application profiles. The local IPC endpoint is not intended to
be a general network service or a complete authentication boundary yet.

## Documentation

Detailed and hardware-specific information lives in [`docs/`](docs/):

- [Feature matrix](docs/feature-matrix.md) - supported capabilities, tested mice, and
  excluded HID++ features.
- [IPC protocol](docs/ipc-protocol.md) - protocol v1 messages, events, persistence,
  and client safety rules.
- [macOS testing](docs/macos-testing.md) - Apple Silicon build, discovery, and
  reversible hardware-validation checklist.
- [Background benchmarking](docs/benchmarking.md) - repeatable native Open Hub versus
  G Hub CPU, memory, I/O, and process-family measurements.

## License

Open Hub is available under the [MIT License](LICENSE).
