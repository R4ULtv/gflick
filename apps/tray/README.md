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

Build both components, then install them as one per-user startup session:

```powershell
cargo build --release -p open-hub-agent -p open-hub-tray
./target/release/open-hub-agent startup install
```

The install command copies both binaries and starts both at login through one Open Hub
registration. Release tray builds use the Windows GUI subsystem, so no console window
remains open. The two processes remain isolated internally for reliability, but their
startup and shutdown behave as one application.

## Runtime design

The tray takes one typed IPC snapshot and then blocks on the agent event subscription.
It does not continuously poll. It wakes for connection lifecycle, device lifecycle,
settings, or battery events. If the agent is unavailable, the icon changes to its
offline presentation and the client retries every five seconds.

The reusable transport lives in `crates/open-hub-client`; future settings applications
can use the same typed request and subscription API instead of duplicating local-socket
framing.

## Initial Windows smoke result

The release tray was validated against the installed Windows agent on August 3, 2026.
After one-time notification-area initialization settled, a full 60-second idle interval
used no measurable CPU time. The process remained stable at five threads, approximately
`2.47 MiB` private memory and `11.98 MiB` working set, with no handle growth. These are
short development measurements, not a final benchmark.

Apple Silicon macOS compilation, menu-bar appearance, title updates, reconnect behavior,
and idle resources remain pending validation.
