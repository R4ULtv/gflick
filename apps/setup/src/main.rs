use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use gflick_install::{
    Bundle, Component, InstallState, InstalledFile, NativePlatform, PlatformBackend, PreparedFile,
    RegistrationState, TransactionPlan, apply, load_state, manifest::STATE_SCHEMA, uninstall,
    validate_installed_state_paths,
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "gflick-setup",
    about = "Install and maintain GFlick for the current user"
)]
struct Cli {
    /// Override the extracted release bundle directory (development/testing only).
    #[arg(long, global = true, hide = true, value_name = "PATH")]
    bundle_root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install, update, or repair GFlick from a verified release bundle.
    Install {
        /// Comma-separated final component set. Defaults come from bundle.json.
        #[arg(long, value_name = "LIST")]
        components: Option<String>,
    },
    /// Add or remove optional components from an existing installation.
    Modify {
        /// Comma-separated final component set.
        #[arg(long, value_name = "LIST")]
        components: String,
    },
    /// Compare installed state with managed files and platform registrations.
    Status {
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
    /// Validate a release bundle without reading or changing installed state.
    VerifyBundle {
        /// Bundle directory; defaults to the setup executable's directory.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
    /// Remove setup-owned installation files and registrations.
    Uninstall {
        /// Also permanently remove GFlick preferences and logs.
        #[arg(long)]
        remove_user_data: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Debug, Serialize)]
struct VerifyReport<'a> {
    valid: bool,
    product_version: &'a str,
    platform: gflick_install::Platform,
    arch: gflick_install::Architecture,
    required_components: Vec<Component>,
    default_components: Vec<Component>,
    available_components: Vec<Component>,
}

#[derive(Debug, Serialize)]
struct StatusReport {
    schema: u32,
    installed: bool,
    installed_version: Option<String>,
    requested_components: Vec<Component>,
    observed_components: Vec<Component>,
    integrity: bool,
    registration: bool,
    path_warning: Option<String>,
    drift: Vec<String>,
}

fn main() -> Result<()> {
    run(Cli::parse())
}

fn run(cli: Cli) -> Result<()> {
    let platform = NativePlatform::new();
    match cli.command {
        Command::VerifyBundle { path, format } => {
            let root = path
                .or(cli.bundle_root)
                .map_or_else(default_bundle_root, Ok)?;
            let bundle = Bundle::open(&root)?.validate()?;
            let manifest = bundle.manifest();
            let report = VerifyReport {
                valid: true,
                product_version: &manifest.product_version,
                platform: manifest.platform,
                arch: manifest.arch,
                required_components: manifest.required.iter().copied().collect(),
                default_components: manifest.defaults.iter().copied().collect(),
                available_components: Component::ALL
                    .into_iter()
                    .filter(|component| manifest.is_available(*component))
                    .collect(),
            };
            print_output(format, &report, || {
                format!(
                    "Bundle {} is valid for {:?}/{:?}. Defaults: {}",
                    report.product_version,
                    report.platform,
                    report.arch,
                    component_list(&report.default_components)
                )
            })
        }
        Command::Status { format } => print_status(&platform, format),
        Command::Install { components } => {
            let root = cli.bundle_root.map_or_else(default_bundle_root, Ok)?;
            install_or_modify(&platform, &root, components.as_deref(), false)
        }
        Command::Modify { components } => {
            let root = cli.bundle_root.map_or_else(default_bundle_root, Ok)?;
            install_or_modify(&platform, &root, Some(&components), true)
        }
        Command::Uninstall { remove_user_data } => {
            let paths = platform.paths()?;
            let Some(state) = load_state(&paths.state_file)? else {
                println!("GFlick is not installed; nothing to remove.");
                return Ok(());
            };
            let preserved = uninstall(&platform, &paths, &state, remove_user_data)?;
            println!("GFlick was removed for the current user.");
            if preserved.is_empty() {
                if remove_user_data {
                    println!("Preferences and logs were removed.");
                }
            } else {
                println!("Preserved user data:");
                for path in preserved {
                    println!("  {}", path.display());
                }
            }
            Ok(())
        }
    }
}

fn install_or_modify(
    platform: &impl PlatformBackend,
    bundle_root: &Path,
    components: Option<&str>,
    require_existing: bool,
) -> Result<()> {
    // Bundle validation intentionally happens before paths, state, IPC shutdown, or
    // any platform registration is touched.
    let bundle = Bundle::open(bundle_root)?.validate()?;
    let manifest = bundle.manifest();
    let selected = components.map_or_else(|| Ok(manifest.defaults.clone()), parse_components)?;
    manifest.validate_selection(&selected)?;

    let paths = platform.paths()?;
    let previous = load_state(&paths.state_file)?;
    if require_existing && previous.is_none() {
        bail!("GFlick is not installed; run `gflick-setup install` first");
    }
    println!(
        "Selected components: {}{}",
        component_list(&selected.iter().copied().collect::<Vec<_>>()),
        if components.is_none() {
            " (bundle defaults)"
        } else {
            ""
        }
    );

    let mut prepared = Vec::new();
    let mut installed = Vec::new();
    let mut wanted_paths = BTreeSet::new();
    for (component, file) in manifest.files_for(&selected) {
        let source = bundle.source(&file.source)?;
        let destination = paths.resolve(file.root).join(&file.destination);
        wanted_paths.insert(destination.clone());
        installed.push(InstalledFile {
            component,
            root: file.root,
            destination: file.destination.clone(),
            length: file.length,
            sha256: file.sha256.clone(),
            executable: file.executable,
        });
        if !installed_file_matches(&destination, file.length, &file.sha256)? {
            prepared.push(PreparedFile {
                component,
                source,
                destination,
                executable: file.executable,
            });
        }
    }
    let remove_paths = previous
        .as_ref()
        .into_iter()
        .flat_map(|state| &state.files)
        .map(|file| paths.resolve(file.root).join(&file.destination))
        .filter(|path| !wanted_paths.contains(path))
        .collect::<Vec<_>>();
    let stop_tray = prepared
        .iter()
        .any(|file| file.component == Some(Component::Tray))
        || previous.as_ref().is_some_and(|state| {
            state.selected_components.contains(&Component::Tray)
                && !selected.contains(&Component::Tray)
        });
    let previous_registrations = previous
        .as_ref()
        .map_or_else(RegistrationState::default, |state| {
            state.registrations.clone()
        });
    let state = InstallState {
        schema: STATE_SCHEMA,
        product_version: manifest.product_version.clone(),
        selected_components: selected.clone(),
        files: installed,
        registrations: RegistrationState::default(),
    };
    let report = apply(
        platform,
        TransactionPlan {
            paths,
            selected_components: selected,
            files: prepared,
            remove_paths,
            previous_registrations,
            new_state: state,
            stop_tray,
        },
    )?;
    println!(
        "GFlick {} is installed. Repaired {} file(s); removed {} obsolete file(s).",
        report.state.product_version, report.repaired_files, report.removed_files
    );
    Ok(())
}

fn print_status(platform: &impl PlatformBackend, format: OutputFormat) -> Result<()> {
    let report = collect_status(platform)?;
    print_output(format, &report, || {
        if !report.installed {
            return "GFlick is not installed.".into();
        }
        let mut text = format!(
            "GFlick {}. Requested: {}. Observed: {}. Integrity: {}.",
            report.installed_version.as_deref().unwrap_or("unknown"),
            component_list(&report.requested_components),
            component_list(&report.observed_components),
            if report.integrity {
                "ok"
            } else {
                "drifted; run install to repair"
            }
        );
        if let Some(warning) = &report.path_warning {
            text.push_str(&format!("\nPATH warning: {warning}"));
        }
        for item in &report.drift {
            text.push_str(&format!("\n  - {item}"));
        }
        text
    })
}

fn collect_status(platform: &impl PlatformBackend) -> Result<StatusReport> {
    let paths = platform.paths()?;
    let Some(state) = load_state(&paths.state_file)? else {
        let observed_registrations = platform.snapshot_registrations()?;
        return Ok(StatusReport {
            schema: 1,
            installed: false,
            installed_version: None,
            requested_components: vec![],
            observed_components: vec![],
            integrity: false,
            registration: false,
            path_warning: observed_registrations.path_warning,
            drift: vec!["installed state is absent".into()],
        });
    };
    validate_installed_state_paths(&paths, &state)?;
    let observed_registrations = platform.snapshot_registrations()?;

    let mut drift = Vec::new();
    let mut observed = BTreeSet::new();
    for file in state.files.iter().filter(|file| file.component.is_none()) {
        let path = paths.resolve(file.root).join(&file.destination);
        if !installed_file_matches(&path, file.length, &file.sha256)? {
            drift.push(format!(
                "setup maintenance payload differs or is missing: {}",
                path.display()
            ));
        }
    }
    for component in &state.selected_components {
        let files = state
            .files
            .iter()
            .filter(|file| file.component == Some(*component));
        let mut count = 0;
        let mut complete = true;
        for file in files {
            count += 1;
            let path = paths.resolve(file.root).join(&file.destination);
            if !installed_file_matches(&path, file.length, &file.sha256)? {
                complete = false;
                drift.push(format!(
                    "managed file differs or is missing: {}",
                    path.display()
                ));
            }
        }
        if count > 0 && complete {
            observed.insert(*component);
        }
    }
    let registration = state.registrations.records.iter().all(|expected| {
        observed_registrations.records.iter().any(|actual| {
            actual.kind == expected.kind
                && actual.location == expected.location
                && actual.value == expected.value
        })
    });
    if !registration {
        drift.push("one or more setup-owned registrations differ or are missing".into());
    }
    let integrity = drift.is_empty();
    Ok(StatusReport {
        schema: 1,
        installed: true,
        installed_version: Some(state.product_version),
        requested_components: state.selected_components.into_iter().collect(),
        observed_components: observed.into_iter().collect(),
        integrity,
        registration,
        path_warning: observed_registrations.path_warning,
        drift,
    })
}

fn parse_components(value: &str) -> Result<BTreeSet<Component>> {
    if value.trim().is_empty() {
        bail!("component list must not be empty");
    }
    value.split(',').map(str::parse).collect()
}

fn component_list(components: &[Component]) -> String {
    components
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn default_bundle_root() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("could not locate the setup executable")?;
    Ok(executable
        .parent()
        .context("setup executable path has no parent")?
        .to_path_buf())
}

fn installed_file_matches(path: &Path, length: u64, sha256: &str) -> Result<bool> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect `{}`", path.display()));
        }
    };
    Ok(metadata.is_file()
        && metadata.len() == length
        && gflick_install::bundle::digest_file(path)? == sha256)
}

fn print_output(
    format: OutputFormat,
    value: &impl Serialize,
    human: impl FnOnce() -> String,
) -> Result<()> {
    match format {
        OutputFormat::Human => println!("{}", human()),
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(value)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use gflick_install::{
        Architecture, BundleFile, BundleManifest, ComponentManifest, InstallRoot, Platform,
        platform::{PlatformPaths, RegistrationKind, RegistrationRecord, RegistrationRequest},
    };
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        fs,
    };

    struct FakePlatform {
        paths: PlatformPaths,
        registrations: RefCell<RegistrationState>,
        snapshot_reads: Cell<usize>,
        shutdowns: Cell<usize>,
        tray_shutdowns: Cell<usize>,
        path_reads: Cell<usize>,
        lifecycle: RefCell<Vec<&'static str>>,
    }

    impl PlatformBackend for FakePlatform {
        fn paths(&self) -> Result<PlatformPaths> {
            self.path_reads.set(self.path_reads.get() + 1);
            Ok(self.paths.clone())
        }

        fn snapshot_registrations(&self) -> Result<RegistrationState> {
            self.snapshot_reads.set(self.snapshot_reads.get() + 1);
            Ok(self.registrations.borrow().clone())
        }

        fn reconcile_registrations(
            &self,
            request: &RegistrationRequest,
            _previous: &RegistrationState,
        ) -> Result<RegistrationState> {
            self.lifecycle.borrow_mut().push("registrations");
            let mut records = Vec::new();
            for (component, kind) in [
                (Component::Agent, RegistrationKind::AgentStartup),
                (Component::Tray, RegistrationKind::TrayStartup),
                (Component::Settings, RegistrationKind::SettingsLauncher),
                (Component::Cli, RegistrationKind::CliExposure),
            ] {
                if request.components.contains(&component) {
                    records.push(RegistrationRecord {
                        kind,
                        location: request.paths.install_root.join(format!("{kind:?}")),
                        value: component.to_string(),
                        owned: true,
                    });
                }
            }
            let state = RegistrationState {
                records,
                path_warning: request
                    .components
                    .contains(&Component::Cli)
                    .then(|| "fake CLI directory is not on PATH".into()),
            };
            *self.registrations.borrow_mut() = state.clone();
            Ok(state)
        }

        fn restore_registrations(&self, snapshot: &RegistrationState) -> Result<()> {
            *self.registrations.borrow_mut() = snapshot.clone();
            Ok(())
        }

        fn request_agent_shutdown(&self) -> Result<()> {
            self.shutdowns.set(self.shutdowns.get() + 1);
            self.lifecycle.borrow_mut().push("agent");
            Ok(())
        }
        fn request_tray_shutdown(&self) -> Result<()> {
            self.tray_shutdowns.set(self.tray_shutdowns.get() + 1);
            self.lifecycle.borrow_mut().push("tray");
            Ok(())
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        bundle: PathBuf,
        platform: FakePlatform,
    }

    fn fixture(settings: bool, future_defaults: bool) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        fs::create_dir_all(bundle.join("payload/settings/assets")).unwrap();
        let setup_name = if cfg!(windows) {
            "gflick-setup.exe"
        } else {
            "gflick-setup"
        };
        let agent_name = if cfg!(windows) {
            "gflick-agent.exe"
        } else {
            "gflick-agent"
        };
        let tray_name = if cfg!(windows) {
            "gflick-tray.exe"
        } else {
            "gflick-tray"
        };
        let cli_name = if cfg!(windows) {
            "gflick.exe"
        } else {
            "gflick"
        };
        for (relative, bytes, executable) in [
            (setup_name, b"setup".as_slice(), true),
            (&format!("payload/{agent_name}"), b"agent".as_slice(), true),
            (&format!("payload/{tray_name}"), b"tray".as_slice(), true),
            ("payload/tray/Info.plist", b"plist".as_slice(), false),
            ("payload/tray/favicon.icns", b"icon".as_slice(), false),
            (&format!("payload/{cli_name}"), b"cli".as_slice(), true),
            (
                "payload/settings/gflick-settings",
                b"settings".as_slice(),
                true,
            ),
            (
                "payload/settings/assets/icon.dat",
                b"icon".as_slice(),
                false,
            ),
        ] {
            let path = bundle.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, bytes).unwrap();
            set_fixture_executable(&path, executable);
        }
        let record =
            |source: &str, root: InstallRoot, destination: &str, executable: bool| -> BundleFile {
                let source_path = bundle.join(source);
                BundleFile {
                    source: source.into(),
                    root,
                    destination: destination.into(),
                    length: fs::metadata(&source_path).unwrap().len(),
                    sha256: gflick_install::bundle::digest_file(&source_path).unwrap(),
                    executable,
                }
            };
        let tray_root = if cfg!(target_os = "macos") {
            InstallRoot::PrivateApp
        } else {
            InstallRoot::PrivateBin
        };
        let tray_destination = if cfg!(target_os = "macos") {
            "Contents/MacOS/gflick-tray"
        } else {
            tray_name
        };
        let mut tray_files = vec![record(
            &format!("payload/{tray_name}"),
            tray_root,
            tray_destination,
            true,
        )];
        if cfg!(target_os = "macos") {
            tray_files.extend([
                record(
                    "payload/tray/Info.plist",
                    InstallRoot::PrivateApp,
                    "Contents/Info.plist",
                    false,
                ),
                record(
                    "payload/tray/favicon.icns",
                    InstallRoot::PrivateApp,
                    "Contents/Resources/favicon.icns",
                    false,
                ),
            ]);
        }
        let mut components = BTreeMap::from([
            (
                Component::Agent,
                ComponentManifest {
                    files: vec![record(
                        &format!("payload/{agent_name}"),
                        InstallRoot::PrivateBin,
                        agent_name,
                        true,
                    )],
                },
            ),
            (Component::Tray, ComponentManifest { files: tray_files }),
            (
                Component::Cli,
                ComponentManifest {
                    files: vec![record(
                        &format!("payload/{cli_name}"),
                        InstallRoot::PrivateBin,
                        cli_name,
                        true,
                    )],
                },
            ),
        ]);
        if settings {
            components.insert(
                Component::Settings,
                ComponentManifest {
                    files: vec![
                        record(
                            "payload/settings/gflick-settings",
                            InstallRoot::UserApplications,
                            if cfg!(target_os = "macos") {
                                "Contents/MacOS/GFlick"
                            } else {
                                "gflick-settings.exe"
                            },
                            true,
                        ),
                        record(
                            "payload/settings/assets/icon.dat",
                            InstallRoot::UserApplications,
                            "assets/icon.dat",
                            false,
                        ),
                    ],
                },
            );
        }
        let mut defaults = BTreeSet::from([Component::Agent, Component::Tray]);
        if future_defaults {
            defaults.insert(Component::Settings);
        }
        let manifest = BundleManifest {
            schema: 1,
            product_version: "2.0.0".into(),
            platform: Platform::current().unwrap(),
            arch: Architecture::current().unwrap(),
            required: BTreeSet::from([Component::Agent]),
            defaults,
            setup: record(setup_name, InstallRoot::PrivateBin, setup_name, true),
            components,
        };
        fs::write(
            bundle.join("bundle.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let install = temp.path().join("installed");
        let paths = PlatformPaths {
            install_root: install.clone(),
            private_bin: install.join("bin"),
            private_app: install.join("GFlick.app"),
            user_applications: temp.path().join("Applications/GFlick.app"),
            user_local_bin: temp.path().join(".local/bin"),
            state_file: install.join("install-state.json"),
            tray_ready: install.join("tray.ready"),
            tray_stop: install.join("tray.stop"),
            preferences: temp.path().join("config/gflick/settings.json"),
            logs: temp.path().join("logs/gflick"),
        };
        Fixture {
            _temp: temp,
            bundle,
            platform: FakePlatform {
                paths,
                registrations: RefCell::new(RegistrationState::default()),
                snapshot_reads: Cell::new(0),
                shutdowns: Cell::new(0),
                tray_shutdowns: Cell::new(0),
                path_reads: Cell::new(0),
                lifecycle: RefCell::new(Vec::new()),
            },
        }
    }

    fn set_fixture_executable(path: &Path, executable: bool) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(if executable { 0o755 } else { 0o644 });
            fs::set_permissions(path, permissions).unwrap();
        }
        #[cfg(not(unix))]
        let _ = (path, executable);
    }

    #[test]
    fn clap_contract_is_sound() {
        Cli::command().debug_assert();
        assert!(Cli::try_parse_from(["setup", "install", "--components", "agent,tray"]).is_ok());
        assert!(Cli::try_parse_from(["setup", "modify", "--components", "agent"]).is_ok());
        assert!(Cli::try_parse_from(["setup", "status", "--format", "json"]).is_ok());
        assert!(
            Cli::try_parse_from(["setup", "verify-bundle", "fixture", "--format", "json"]).is_ok()
        );
        assert!(Cli::try_parse_from(["setup", "uninstall", "--remove-user-data"]).is_ok());
        assert!(Cli::try_parse_from(["setup", "modify"]).is_err());
    }

    #[test]
    fn component_parser_is_closed_and_requires_agent_at_model_boundary() {
        assert_eq!(
            parse_components("agent,cli").unwrap(),
            BTreeSet::from([Component::Agent, Component::Cli])
        );
        assert!(parse_components("probe").is_err());
        assert!(parse_components("bench").is_err());
        assert!(parse_components("").is_err());
    }

    #[test]
    fn orchestrates_current_defaults_headless_all_components_repair_and_status() {
        let fixture = fixture(false, false);
        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        let state = load_state(&fixture.platform.paths.state_file)
            .unwrap()
            .unwrap();
        assert_eq!(
            state.selected_components,
            BTreeSet::from([Component::Agent, Component::Tray])
        );
        assert_eq!(
            fixture.platform.lifecycle.borrow().as_slice(),
            ["tray", "agent", "registrations"]
        );
        assert!(state.files.iter().any(|file| file.component.is_none()));
        assert!(collect_status(&fixture.platform).unwrap().integrity);

        let agent = state
            .files
            .iter()
            .find(|file| file.component == Some(Component::Agent))
            .unwrap();
        let agent_path = fixture
            .platform
            .paths
            .resolve(agent.root)
            .join(&agent.destination);
        fs::write(&agent_path, b"drift").unwrap();
        assert!(!collect_status(&fixture.platform).unwrap().integrity);
        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        assert!(collect_status(&fixture.platform).unwrap().integrity);

        install_or_modify(&fixture.platform, &fixture.bundle, Some("agent"), true).unwrap();
        assert_eq!(
            load_state(&fixture.platform.paths.state_file)
                .unwrap()
                .unwrap()
                .selected_components,
            BTreeSet::from([Component::Agent])
        );
        install_or_modify(
            &fixture.platform,
            &fixture.bundle,
            Some("agent,tray,cli"),
            true,
        )
        .unwrap();
        install_or_modify(&fixture.platform, &fixture.bundle, Some("agent,cli"), true).unwrap();
        let final_state = load_state(&fixture.platform.paths.state_file)
            .unwrap()
            .unwrap();
        assert_eq!(
            final_state.selected_components,
            BTreeSet::from([Component::Agent, Component::Cli])
        );
        assert!(fixture.platform.shutdowns.get() >= 5);
        assert!(fixture.platform.tray_shutdowns.get() >= 3);
    }

    #[test]
    fn status_reports_corrupt_and_missing_setup_maintenance_payload() {
        let fixture = fixture(false, false);
        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        let state = load_state(&fixture.platform.paths.state_file)
            .unwrap()
            .unwrap();
        let setup = state
            .files
            .iter()
            .find(|file| file.component.is_none())
            .unwrap();
        let setup_path = fixture
            .platform
            .paths
            .resolve(setup.root)
            .join(&setup.destination);

        fs::write(&setup_path, b"corrupt").unwrap();
        let corrupt = collect_status(&fixture.platform).unwrap();
        assert!(!corrupt.integrity);
        assert!(
            corrupt
                .drift
                .iter()
                .any(|item| item.contains("setup maintenance payload differs or is missing"))
        );

        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        fs::remove_file(&setup_path).unwrap();
        let missing = collect_status(&fixture.platform).unwrap();
        assert!(!missing.integrity);
        assert!(
            missing
                .drift
                .iter()
                .any(|item| item.contains("setup maintenance payload differs or is missing"))
        );
    }

    #[test]
    fn status_rejects_a_selected_component_without_managed_files() {
        let fixture = fixture(false, false);
        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        let mut state = load_state(&fixture.platform.paths.state_file)
            .unwrap()
            .unwrap();
        state
            .files
            .retain(|file| file.component != Some(Component::Tray));
        fs::write(
            &fixture.platform.paths.state_file,
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();

        let error = collect_status(&fixture.platform).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("selects component `tray` without any managed files")
        );
    }

    #[test]
    fn orchestrates_future_settings_tree_launcher_and_removal_without_login_item() {
        let fixture = fixture(true, true);
        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        let state = load_state(&fixture.platform.paths.state_file)
            .unwrap()
            .unwrap();
        assert_eq!(
            state.selected_components,
            BTreeSet::from([Component::Agent, Component::Settings, Component::Tray])
        );
        let settings_files = state
            .files
            .iter()
            .filter(|file| file.component == Some(Component::Settings))
            .collect::<Vec<_>>();
        assert_eq!(settings_files.len(), 2);
        assert!(settings_files.iter().all(|file| {
            fixture
                .platform
                .paths
                .resolve(file.root)
                .join(&file.destination)
                .is_file()
        }));
        assert!(
            state
                .registrations
                .record(RegistrationKind::SettingsLauncher)
                .is_some()
        );
        assert_eq!(
            state
                .registrations
                .records
                .iter()
                .filter(|record| matches!(
                    record.kind,
                    RegistrationKind::AgentStartup | RegistrationKind::TrayStartup
                ))
                .count(),
            2
        );
        install_or_modify(&fixture.platform, &fixture.bundle, Some("agent"), true).unwrap();
        assert!(settings_files.iter().all(|file| {
            !fixture
                .platform
                .paths
                .resolve(file.root)
                .join(&file.destination)
                .exists()
        }));
        assert!(!fixture.platform.paths.user_applications.exists());
        assert!(
            fixture
                .platform
                .registrations
                .borrow()
                .record(RegistrationKind::SettingsLauncher)
                .is_none()
        );
    }

    #[test]
    fn settings_removal_preserves_unrelated_application_sentinel() {
        let fixture = fixture(true, true);
        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        let sentinel = fixture
            .platform
            .paths
            .user_applications
            .join("user-created.txt");
        fs::write(&sentinel, b"keep").unwrap();
        install_or_modify(&fixture.platform, &fixture.bundle, Some("agent"), true).unwrap();
        assert_eq!(fs::read(sentinel).unwrap(), b"keep");
    }

    #[test]
    fn rejects_unavailable_settings_and_no_agent_before_environment_mutation() {
        for components in ["agent,settings", "tray", "probe", "bench"] {
            let fixture = fixture(false, false);
            assert!(
                install_or_modify(&fixture.platform, &fixture.bundle, Some(components), false)
                    .is_err()
            );
            assert_eq!(fixture.platform.shutdowns.get(), 0);
            assert_eq!(fixture.platform.path_reads.get(), 0);
            assert!(!fixture.platform.paths.state_file.exists());
        }
    }

    #[test]
    fn uninstall_preserves_data_normally_and_removes_it_only_when_explicit() {
        for remove_user_data in [false, true] {
            let fixture = fixture(false, false);
            install_or_modify(&fixture.platform, &fixture.bundle, Some("agent"), false).unwrap();
            fs::create_dir_all(fixture.platform.paths.preferences.parent().unwrap()).unwrap();
            fs::write(&fixture.platform.paths.preferences, b"preferences").unwrap();
            fs::create_dir_all(&fixture.platform.paths.logs).unwrap();
            fs::write(fixture.platform.paths.logs.join("agent.log"), b"log").unwrap();
            let state = load_state(&fixture.platform.paths.state_file)
                .unwrap()
                .unwrap();
            let preserved = uninstall(
                &fixture.platform,
                &fixture.platform.paths,
                &state,
                remove_user_data,
            )
            .unwrap();
            assert_eq!(
                fixture.platform.paths.preferences.exists(),
                !remove_user_data
            );
            assert_eq!(fixture.platform.paths.logs.exists(), !remove_user_data);
            assert_eq!(preserved.is_empty(), remove_user_data);
            assert!(!fixture.platform.paths.state_file.exists());
        }
    }

    #[test]
    fn status_reports_registration_drift() {
        let fixture = fixture(false, false);
        install_or_modify(&fixture.platform, &fixture.bundle, None, false).unwrap();
        fixture
            .platform
            .registrations
            .borrow_mut()
            .records
            .retain(|record| record.kind != RegistrationKind::TrayStartup);
        let status = collect_status(&fixture.platform).unwrap();
        assert!(!status.registration);
        assert!(!status.integrity);
        assert!(
            status
                .drift
                .iter()
                .any(|item| item.contains("registrations"))
        );
    }

    #[test]
    fn status_rejects_an_outside_state_target_before_observing_platform_state() {
        let fixture = fixture(false, false);
        install_or_modify(&fixture.platform, &fixture.bundle, Some("agent"), false).unwrap();
        let outside = fixture._temp.path().join("outside-sentinel");
        fs::write(&outside, b"untouched").unwrap();
        let mut state = load_state(&fixture.platform.paths.state_file)
            .unwrap()
            .unwrap();
        state.files[0].destination = outside.clone();
        fs::write(
            &fixture.platform.paths.state_file,
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        let reads_before = fixture.platform.snapshot_reads.get();

        assert!(collect_status(&fixture.platform).is_err());
        assert_eq!(fixture.platform.snapshot_reads.get(), reads_before);
        assert_eq!(fs::read(outside).unwrap(), b"untouched");
    }
}
