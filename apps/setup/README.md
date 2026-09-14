# GFlick Setup

`gflick-setup` is the per-user, non-elevated installer and maintenance tool. It
installs a verified release bundle that is already present on disk; it does not
download components from the network.

## Get a release bundle

Artifacts are currently **unsigned and experimental**. From a GitHub prerelease,
download `SHA256SUMS` and exactly one user archive for your platform:

- `gflick-<version>-windows-x86_64.zip` for Windows 10/11 x86_64;
- `gflick-<version>-macos-aarch64.tar.gz` for Apple Silicon macOS.

Verify the downloaded archive against its `SHA256SUMS` entry before extracting it.
On Windows, use `Get-FileHash <archive> -Algorithm SHA256`; on macOS, use
`shasum -a 256 <archive>`. Extract it and run `gflick-setup install` from that
directory. No Rust toolchain or source build is required.

Developer tools are published separately as
`gflick-devtools-<version>-<platform>-<arch>`. They are not installable components:
Probe opens HID directly and must not run concurrently with the agent.

## Bundle layout

An extracted release has this shape:

```text
gflick-<version>-<platform>-<arch>/
  gflick-setup[.exe]
  bundle.json
  payload/
    gflick-agent[.exe]
    gflick-tray[.exe]
    gflick[.exe]
    settings/                 # present only in bundles packaged with an app tree
      ...
```

Before changing the installation, setup validates the manifest schema, platform,
architecture, component dependencies, relative paths, byte lengths, SHA-256 digests,
and executable metadata for every payload file. Manifest destinations are restricted
to known per-user roots. Absolute paths and parent traversal are rejected.

You can validate an extracted bundle without reading or changing installed state:

```sh
gflick-setup verify-bundle
gflick-setup verify-bundle /path/to/extracted/release --format json
```

## Components

| Component | Availability | Purpose |
| --- | --- | --- |
| `agent` | Required | Owns devices, settings persistence, and local IPC |
| `settings` | Packaging-ready, not currently shipped | Native desktop interface; unavailable in a bundle unless it contains the application payload |
| `tray` | Optional, currently default | Shows device status in the notification area or menu bar |
| `cli` | Optional | Provides the `gflick` terminal interface |

The current published-bundle default is `agent,tray`. A headless installation can
select only `agent`, and an installation with every component in those bundles uses
`agent,tray,cli`. `probe` and `bench` are developer artifacts and are not installable
components. The Settings client exists in the source workspace, but the release
workflow does not package it yet. A custom bundle built with a verified Settings
application tree adds `settings` to its available components and defaults.

Install the manifest defaults or choose a component set:

```sh
gflick-setup install
gflick-setup install --components agent
gflick-setup install --components agent,tray,cli
```

The agent is always required. Setup rejects a final component set without it and
rejects `settings` when the current bundle has no Settings payload. Re-running
`install` repairs drift and updates files idempotently. Change optional components
later with:

```sh
gflick-setup modify --components agent,cli
```

## Installed locations

Windows installs private binaries below `%LOCALAPPDATA%\gflick\bin`. A packaged
Settings application is installed below `%LOCALAPPDATA%\Programs\GFlick`. The agent and
tray have independent `HKCU` login registrations; settings is not a login item. When
the CLI is selected, setup adds the exact private bin directory to the current user's
`PATH` only when it is absent and records ownership of that entry.

Apple Silicon macOS installs private data below
`~/Library/Application Support/gflick`, the tray as its private `GFlick.app`, and a
packaged Settings application as `~/Applications/GFlick.app`. The agent and tray
have separate LaunchAgents; settings is not a LaunchAgent. The CLI is exposed as
`~/.local/bin/gflick` without editing shell startup files. Status warns when
`~/.local/bin` is not already on `PATH`.

Intel macOS is unsupported and setup rejects it with an architecture error.

## Status, repair, and removal

Inspect the requested component set, observed files, registrations, and integrity:

```sh
gflick-setup status
gflick-setup status --format json
```

Human output identifies drift and recommends running `install` to repair it. The JSON
form is stable for automation. Setup keeps an atomic installed-state document as a
recovery aid, but status verifies the actual files and registrations rather than
trusting state alone.

Normal uninstall removes setup-owned files, login registrations, and only the
CLI `PATH` entry or symlink that setup owns. Device preferences and logs are retained:

```sh
gflick-setup uninstall
```

Remove preferences and logs only with the explicit destructive acknowledgement:

```sh
gflick-setup uninstall --remove-user-data
```

Interrupted installation is recovered transactionally: payload files are staged and
flushed before replacement, prior files and registrations are retained until the new
installation succeeds, and installed state is persisted last.

Before replacing managed executables, setup asks the agent to shut down through the
existing local IPC protocol and asks the tray to exit through a bounded per-user
generation-token handshake. The tray publishes `tray.ready`; setup writes `tray.stop`
only with that exact live token and waits for the tray to consume it. An offline or
absent process is not an error. Both sides remove only matching tokens, so a stale
request cannot stop a later tray generation.
