# Open Hub CLI

`open-hub` is the user-facing command-line client for a running Open Hub background
agent. It sends requests over the local IPC endpoint; it never opens HID devices
directly. Start or install the agent before using this command.

## Commands

```text
open-hub [--format human|json] <COMMAND>
  status
  device list
  device show [SELECTOR]
  device set [SELECTOR] dpi <DPI>
  device set [SELECTOR] polling-rate <HZ> [--connection wired|wireless]
                                           [--disable-onboard-profiles]
  device set [SELECTOR] lift-off-distance <low|medium|high>
  device set [SELECTOR] surface-mode <on|automatic|off>
  device set [SELECTOR] operating-mode <performance|endurance>
  device set [SELECTOR] bhop <on|off> [--window-ms <100..=1000>]
  device set [SELECTOR] lighting off
  device set [SELECTOR] lighting fixed --color <RRGGBB>
  device set [SELECTOR] lighting cycling [--period-ms N] [--brightness 1..=100]
  device set [SELECTOR] lighting breathing --color <RRGGBB>
                                            [--period-ms N] [--brightness 1..=100]
  device use [SELECTOR] firmware-lighting
  device use [SELECTOR] host-settings
  device use [SELECTOR] onboard-profile <SECTOR>
  events [--count N]
  completions <bash|elvish|fish|powershell|zsh>
```

`bhop` uses a 100 ms window by default. Lighting cycling and breathing use a 1000 ms
period and 100 brightness by default. `--color` accepts six hexadecimal digits, such
as `ff6600` or `#ff6600`. `events --count` must be at least 1.

## Device selectors

Before showing, changing, or selecting a device mode, the CLI fetches the current
device list from the agent. A selector is one of:

- an exact session ID;
- an exact hardware ID; or
- a unique, case-insensitive prefix of either ID.

The CLI always routes the resulting command with the current session ID. Device names
are deliberately not selectors. If no selector is given, the command proceeds only
when exactly one device is listed. With multiple devices it prints candidate names and
stable selectors; it never silently chooses the first device.

## Output and exit codes

Human-readable output is the default. `--format json` writes JSON only to standard
output: `device list` writes `{ "devices": [...] }`; `device show` and device
mutations write `{ "device": ... }`; `status` reports the agent, protocol, and CLI
version; and `events` writes one event JSON object per line. Diagnostics and errors go
to standard error. Human device lists include a stable selector and lifecycle status;
device snapshots include only the settings currently reported by a ready device.

Successful commands exit 0. Invalid command-line usage exits 2. Other failures,
including an unreachable agent, exit 1. If the agent is not running, start or install
the Open Hub background agent; the CLI does not fall back to direct HID access.

## Examples

```sh
open-hub status
open-hub device list
open-hub device show 046d:unit
open-hub device set 046d:unit dpi 1600
open-hub device set 046d:unit polling-rate 1000 --connection wireless
open-hub device set 046d:unit lighting fixed --color ff6600
open-hub --format json events --count 1
open-hub completions powershell > open-hub.ps1
```

`open-hub-probe` remains the developer diagnostic tool, including direct-HID and
dangerous onboard-flash work. Do not run Probe's direct-HID commands concurrently with
the agent or `open-hub`.
