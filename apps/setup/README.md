# Open Hub Setup

`open-hub-setup` is the per-user, non-elevated installer and maintenance tool. It
installs a verified release bundle that is already present on disk; it does not
download components from the network.

## Get a release bundle

Artifacts are currently **unsigned and experimental**. From a GitHub prerelease,
download `SHA256SUMS` and exactly one user archive for your platform:

- `open-hub-<version>-windows-x86_64.zip` for Windows 10/11 x86_64;
- `open-hub-<version>-macos-aarch64.tar.gz` for Apple Silicon macOS.

Verify the downloaded archive against its `SHA256SUMS` entry before extracting it.
On Windows, use `Get-FileHash <archive> -Algorithm SHA256`; on macOS, use
`shasum -a 256 <archive>`. Extract it and run `open-hub-setup install` from that
directory. No Rust toolchain or source build is required.

Developer tools are published separately as
`open-hub-devtools-<version>-<platform>-<arch>`. They are not installable components:
Probe opens HID directly and must not run concurrently with the agent.

## Bundle layout

An extracted release has this shape:

```text
open-hub-<version>-<platform>-<arch>/
  open-hub-setup[.exe]
  bundle.json
  payload/
    open-hub-agent[.exe]
    open-hub-tray[.exe]
    open-hub[.exe]
    settings/                 # present only after the settings app ships
      ...
```

Before changing the installation, setup validates the manifest schema, platform,
architecture, component dependencies, relative paths, byte lengths, SHA-256 digests,
and executable metadata for every payload file. Manifest destinations are restricted
to known per-user roots. Absolute paths and parent traversal are rejected.

You can validate an extracted bundle without reading or changing installed state:

```sh
open-hub-setup verify-bundle
open-hub-setup verify-bundle /path/to/extracted/release --format json
```

## Components

| Component | Availability | Purpose |
| --- | --- | --- |
| `agent` | Required | Owns devices, settings persistence, and local IPC |
| `settings` | Reserved | Future primary desktop interface; unavailable until its application payload ships |
| `tray` | Optional, currently default | Shows device status in the notification area or menu bar |
| `cli` | Optional | Provides the `open-hub` terminal interface |

The current fresh-install default is `agent,tray`. A headless installation can select
only `agent`, and an installation with every currently available user component uses
`agent,tray,cli`. `probe` and `bench` are developer artifacts and are not installable
components. When the settings application ships, its verified payload can make the
desktop default `agent,settings,tray` without changing the agent.

Install the manifest defaults or choose a component set:

```sh
open-hub-setup install
open-hub-setup install --components agent
open-hub-setup install --components agent,tray,cli
```

The agent is always required. Setup rejects a final component set without it and
rejects `settings` while its payload is absent. Re-running `install` repairs drift and
updates files idempotently. Change optional components later with:

```sh
open-hub-setup modify --components agent,cli
```

## Installed locations

Windows installs private binaries below `%LOCALAPPDATA%\open-hub\bin`. The future
settings application belongs below `%LOCALAPPDATA%\Programs\Open Hub`. The agent and
tray have independent `HKCU` login registrations; settings is not a login item. When
the CLI is selected, setup adds the exact private bin directory to the current user's
`PATH` only when it is absent and records ownership of that entry.

Apple Silicon macOS installs private data below
`~/Library/Application Support/open-hub`, the tray as its private `Open Hub.app`, and
the future settings application as `~/Applications/Open Hub.app`. The agent and tray
have separate LaunchAgents; settings is not a LaunchAgent. The CLI is exposed as
`~/.local/bin/open-hub` without editing shell startup files. Status warns when
`~/.local/bin` is not already on `PATH`.

Intel macOS is unsupported and setup rejects it with an architecture error.

## Status, repair, and removal

Inspect the requested component set, observed files, registrations, and integrity:

```sh
open-hub-setup status
open-hub-setup status --format json
```

Human output identifies drift and recommends running `install` to repair it. The JSON
form is stable for automation. Setup keeps an atomic installed-state document as a
recovery aid, but status verifies the actual files and registrations rather than
trusting state alone.

Normal uninstall removes setup-owned files, login registrations, and only the
CLI `PATH` entry or symlink that setup owns. Device preferences and logs are retained:

```sh
open-hub-setup uninstall
```

Remove preferences and logs only with the explicit destructive acknowledgement:

```sh
open-hub-setup uninstall --remove-user-data
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
