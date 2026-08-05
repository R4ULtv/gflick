# Open Hub tray

`open-hub-tray` is a lightweight native companion to the headless agent. It is a
notification-area app on Windows and a menu-bar status item on macOS. It never opens HID
devices and cannot change mouse settings; all state comes from the versioned local IPC
protocol.

The menu shows:

- whether the background agent is connected;
- every mouse currently known to the agent;
- current battery percentage and charging state;
- current X/Y DPI; and
- unavailable or disconnected states without displaying stale readings.

The macOS status-item title also shows the primary mouse's battery and DPI without
opening the menu. On Windows the same summary is available by hovering over the tray
icon. `Refresh` requests a fresh device snapshot. `Quit Open Hub` gracefully stops the
agent and closes the tray, matching the lifecycle users expect from the visible app.

Windows uses the multi-resolution `assets/favicon.ico` for the executable and tray.
macOS installs a background `Open Hub.app` bundle with `assets/favicon.icns`, while its
menu-bar item uses the monochrome, transparent `assets/tray-template.png` so the system
can adapt it to light, dark, and highlighted appearances.

## Build and run

Build the tray during development, or select it from an extracted release bundle with
the setup bootstrapper:

```powershell
cargo build --release -p open-hub-tray
open-hub-setup install --components agent,tray
```

The agent and tray use independent per-user login registrations, so tray remains
optional and a headless agent install is supported. Release tray builds use the Windows
GUI subsystem, so no console window remains open. See the
[setup README](../setup/README.md) for platform paths and maintenance.

## Runtime design

The tray takes one typed IPC snapshot and then blocks on the agent event subscription.
It does not continuously poll. It wakes for connection lifecycle, device lifecycle,
settings, or battery events. If the agent is unavailable, the icon changes to its
offline presentation and the client retries every five seconds.

The reusable transport lives in `crates/open-hub-client`; future settings applications
can use the same typed request and subscription API instead of duplicating local-socket
framing.

During repair, update, or removal, `open-hub-setup` uses a bounded per-user stop-request
handshake so the tray can exit cleanly even while the agent is offline. The tray
consumes the request and closes through its normal event-loop path; setup never
force-kills it and cleans an unconsumed request when no tray is running.

## Platform validation

The release tray was validated against the installed Windows agent on August 3, 2026.
After one-time notification-area initialization settled, a full 60-second idle interval
used no measurable CPU time. The process remained stable at five threads, approximately
`2.47 MiB` private memory and `11.98 MiB` working set, with no handle growth. These are
short development measurements, not a final benchmark.

Apple Silicon macOS validation passed on a MacBook Air with an Apple M4 and macOS
15.7.7. The installed application bundle, per-user startup, accessory activation policy,
full-color Finder icon, template menu-bar icon, battery/DPI title, live menu updates,
wireless unavailable/recovery events, and application-wide quit behavior all worked as
intended. The agent and tray also remained stable throughout a complete 30-minute idle
recording; the combined measurements and limitations are documented in the
[benchmark README](../bench/README.md#initial-apple-silicon-macos-result).
