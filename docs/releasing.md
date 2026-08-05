# Releasing GFlick

GFlick releases are tag-driven prereleases containing unsigned, experimental
Windows x86_64 and Apple Silicon macOS bundles. The setup bootstrapper only verifies
and installs an extracted local bundle; it never downloads release code.

## Prepare the version

Choose an unprefixed semantic version such as `0.1.0`. Set that same version in all
six application manifests: agent, tray, CLI, setup, Probe, and Bench. The release
workflow rejects the tag unless every package version exactly matches it.

Run the complete local gates from a clean checkout:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked -p gflick-agent -p gflick-tray -p gflick-cli -p gflick-setup -p gflick-probe -p gflick-bench
git diff --check
```

Open a pull request. CI must pass on Windows and the `macos-15` ARM64 runner before
the first release is considered ready. The macOS jobs explicitly reject a runner
whose `uname -m` is not `arm64`.

## Package locally

The platform scripts accept only trusted, locally built binaries. They generate the
schema-1 manifest after staging final payloads, invoke the non-mutating setup verifier,
and keep developer tools outside the user bundle.

```powershell
.\scripts\package-release.ps1 -Version 0.1.0 -Target windows -Architecture x86_64 -BinaryDir target\release -OutputDir dist
```

```sh
./scripts/package-release.sh --version 0.1.0 --target macos --arch aarch64 --binary-dir target/release --output-dir dist
```

Pass `-SettingsAppDir <directory>` or `--settings-app-dir <directory>` only after a
real settings application tree exists. Every regular file is then represented below
the closed `user_applications` root and settings becomes a default component. Omitting
the argument produces no settings entry or placeholder.

## Tag and publish

After merging the validated version change, create and push `v<version>` from the
exact commit to release:

```sh
git tag -a v0.1.0 -m "GFlick v0.1.0"
git push origin v0.1.0
```

Ordinary pushes and pull requests never publish. The tag workflow builds the same six
packages on both supported platforms and creates these five prerelease assets:

```text
gflick-0.1.0-windows-x86_64.zip
gflick-0.1.0-macos-aarch64.tar.gz
gflick-devtools-0.1.0-windows-x86_64.zip
gflick-devtools-0.1.0-macos-aarch64.tar.gz
SHA256SUMS
```

The user archives contain setup, `bundle.json`, agent, tray, and CLI payloads. The
developer archives contain Probe, Bench, and a direct-HID concurrency warning. Only
the final publish job has `contents: write`; it refuses to replace an existing release.

## Validate the prerelease

Download all five assets into an empty directory and verify every external digest:

```sh
sha256sum -c SHA256SUMS
```

On macOS use `shasum -a 256` on each archive and compare it with `SHA256SUMS`. Extract
each user archive and run `gflick-setup verify-bundle` before a clean per-user install.
Confirm install, status, modify, repair, and uninstall on Windows 10/11 x86_64 and on
Apple Silicon macOS. Also inspect archive listings to ensure Probe and Bench occur only
in developer archives and that the macOS tray contains its executable, `Info.plist`,
and icon.

## Yank or roll back

If validation fails, do not reuse or move the tag. Delete the prerelease assets and
release, delete the remote tag, and document why it was yanked. Fix the issue, bump to
a new patch version, repeat all gates, and publish a new tag. Users who already
installed can run `gflick-setup uninstall`; normal uninstall retains preferences and
logs unless they explicitly request user-data removal.
