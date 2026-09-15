//! Windows per-user registration implementation.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;

use super::{
    PlatformBackend, PlatformPaths, RegistrationKind, RegistrationRecord, RegistrationRequest,
    RegistrationState,
};
use crate::manifest::Component;

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const ENVIRONMENT_KEY: &str = r"HKCU\Environment";
const AGENT_VALUE: &str = "gflickAgent";
const TRAY_VALUE: &str = "gflickTray";
const PATH_VALUE: &str = "Path";

#[derive(Clone, Debug, Default)]
pub struct WindowsPlatform;

impl WindowsPlatform {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(windows)]
impl PlatformBackend for WindowsPlatform {
    fn paths(&self) -> Result<PlatformPaths> {
        let base = BaseDirs::new().context("could not determine per-user Windows directories")?;
        let root = base.data_local_dir().join("gflick");
        Ok(PlatformPaths {
            install_root: root.clone(),
            private_bin: root.join("bin"),
            private_app: root.join("app"),
            user_applications: base.data_local_dir().join("Programs/gflick"),
            user_local_bin: root.join("bin"),
            state_file: root.join("install-state.json"),
            tray_ready: root.join("tray.ready"),
            tray_stop: root.join("tray.stop"),
            preferences: base.config_dir().join("gflick/settings.json"),
            logs: root.join("logs"),
        })
    }

    fn snapshot_registrations(&self) -> Result<RegistrationState> {
        let paths = self.paths()?;
        let mut records = Vec::new();
        if let Some(value) = query_registry_value(RUN_KEY, AGENT_VALUE)? {
            records.push(record(
                RegistrationKind::AgentStartup,
                registry_location(RUN_KEY, AGENT_VALUE),
                value,
                false,
            ));
        }
        if let Some(value) = query_registry_value(RUN_KEY, TRAY_VALUE)? {
            records.push(record(
                RegistrationKind::TrayStartup,
                registry_location(RUN_KEY, TRAY_VALUE),
                value,
                false,
            ));
        }
        let shortcut = settings_shortcut()?;
        if shortcut.is_file() {
            records.push(record(
                RegistrationKind::SettingsLauncher,
                shortcut.clone(),
                read_shortcut_target(&shortcut)?,
                false,
            ));
        }
        let path = query_registry_value(ENVIRONMENT_KEY, PATH_VALUE)?.unwrap_or_default();
        if path_contains(&path, &paths.private_bin) {
            records.push(record(
                RegistrationKind::CliExposure,
                registry_location(ENVIRONMENT_KEY, PATH_VALUE),
                paths.private_bin.to_string_lossy().into_owned(),
                false,
            ));
        }
        Ok(RegistrationState {
            records,
            path_warning: None,
        })
    }

    fn reconcile_registrations(
        &self,
        request: &RegistrationRequest,
        previous: &RegistrationState,
    ) -> Result<RegistrationState> {
        let mut result = RegistrationState::default();
        reconcile_run_value(
            Component::Agent,
            RegistrationKind::AgentStartup,
            AGENT_VALUE,
            &agent_command(&request.paths),
            request,
            previous,
            &mut result,
        )?;
        reconcile_run_value(
            Component::Tray,
            RegistrationKind::TrayStartup,
            TRAY_VALUE,
            &tray_command(&request.paths),
            request,
            previous,
            &mut result,
        )?;

        let shortcut = settings_shortcut()?;
        if request.components.contains(&Component::Settings) {
            let target = settings_executable(&request.paths);
            let target_value = target.to_string_lossy().into_owned();
            match previous.record(RegistrationKind::SettingsLauncher) {
                Some(existing) if !existing.owned => {
                    bail!(
                        "refusing to overwrite pre-existing settings shortcut `{}`",
                        existing.location.display()
                    );
                }
                _ => {
                    write_shortcut(&shortcut, &target)?;
                    result.records.push(record(
                        RegistrationKind::SettingsLauncher,
                        shortcut,
                        target_value,
                        true,
                    ));
                }
            }
        } else {
            remove_owned_file(previous, RegistrationKind::SettingsLauncher)?;
        }

        reconcile_path(request, previous, &mut result)?;
        Ok(result)
    }

    fn restore_registrations(&self, snapshot: &RegistrationState) -> Result<()> {
        restore_run_value(snapshot, RegistrationKind::AgentStartup, AGENT_VALUE)?;
        restore_run_value(snapshot, RegistrationKind::TrayStartup, TRAY_VALUE)?;

        let shortcut = settings_shortcut()?;
        if let Some(old) = snapshot.record(RegistrationKind::SettingsLauncher) {
            write_shortcut(&shortcut, Path::new(&old.value))?;
        } else if shortcut.is_file() {
            fs::remove_file(&shortcut).with_context(|| {
                format!(
                    "failed to remove settings shortcut `{}`",
                    shortcut.display()
                )
            })?;
        }

        let mut path = query_registry_value(ENVIRONMENT_KEY, PATH_VALUE)?.unwrap_or_default();
        let bin = self.paths()?.private_bin;
        let wanted = snapshot.record(RegistrationKind::CliExposure).is_some();
        let present = path_contains(&path, &bin);
        if wanted && !present {
            path = append_path(&path, &bin);
            set_registry_value(ENVIRONMENT_KEY, PATH_VALUE, &path)?;
        } else if !wanted && present {
            path = remove_path(&path, &bin);
            set_registry_value(ENVIRONMENT_KEY, PATH_VALUE, &path)?;
        }
        Ok(())
    }
}

fn record(
    kind: RegistrationKind,
    location: PathBuf,
    value: String,
    owned: bool,
) -> RegistrationRecord {
    RegistrationRecord {
        kind,
        location,
        value,
        owned,
    }
}

fn registry_location(key: &str, name: &str) -> PathBuf {
    PathBuf::from(format!(r"{key}\{name}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistrationDecision {
    WriteOwned,
    PreserveUnowned,
}

fn registration_decision(
    previous: &RegistrationState,
    kind: RegistrationKind,
    expected: &str,
) -> Result<RegistrationDecision> {
    match previous.record(kind) {
        Some(existing) if !existing.owned && !existing.value.eq_ignore_ascii_case(expected) => {
            bail!(
                "refusing to overwrite pre-existing registration `{}`",
                existing.location.display()
            )
        }
        Some(existing) if !existing.owned => Ok(RegistrationDecision::PreserveUnowned),
        _ => Ok(RegistrationDecision::WriteOwned),
    }
}

fn agent_command(paths: &PlatformPaths) -> String {
    format!(
        r#""{}" --background-worker"#,
        paths.private_bin.join("gflick-agent.exe").display()
    )
}

fn tray_command(paths: &PlatformPaths) -> String {
    format!(
        r#""{}""#,
        paths.private_bin.join("gflick-tray.exe").display()
    )
}

fn settings_executable(paths: &PlatformPaths) -> PathBuf {
    paths.user_applications.join("gflick-settings.exe")
}

fn settings_shortcut() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine per-user Windows directories")?;
    let app_data = base
        .data_dir()
        .parent()
        .map_or_else(|| base.data_dir().to_path_buf(), Path::to_path_buf);
    Ok(app_data.join("Roaming/Microsoft/Windows/Start Menu/Programs/gflick.lnk"))
}

#[cfg(windows)]
fn reconcile_run_value(
    component: Component,
    kind: RegistrationKind,
    name: &str,
    command: &str,
    request: &RegistrationRequest,
    previous: &RegistrationState,
    output: &mut RegistrationState,
) -> Result<()> {
    if request.components.contains(&component) {
        match registration_decision(previous, kind, command)? {
            RegistrationDecision::PreserveUnowned => output.records.push(record(
                kind,
                registry_location(RUN_KEY, name),
                command.to_owned(),
                false,
            )),
            RegistrationDecision::WriteOwned => {
                set_registry_value(RUN_KEY, name, command)?;
                output.records.push(record(
                    kind,
                    registry_location(RUN_KEY, name),
                    command.to_owned(),
                    true,
                ));
            }
        }
    } else if previous.record(kind).is_some_and(|record| record.owned) {
        delete_registry_value(RUN_KEY, name)?;
    }
    Ok(())
}

#[cfg(windows)]
fn restore_run_value(
    snapshot: &RegistrationState,
    kind: RegistrationKind,
    name: &str,
) -> Result<()> {
    if let Some(old) = snapshot.record(kind) {
        set_registry_value(RUN_KEY, name, &old.value)
    } else {
        delete_registry_value(RUN_KEY, name)
    }
}

#[cfg(windows)]
fn reconcile_path(
    request: &RegistrationRequest,
    previous: &RegistrationState,
    output: &mut RegistrationState,
) -> Result<()> {
    let mut path = query_registry_value(ENVIRONMENT_KEY, PATH_VALUE)?.unwrap_or_default();
    let present = path_contains(&path, &request.paths.private_bin);
    if request.components.contains(&Component::Cli) {
        let owned = if present {
            cli_entry_is_owned(previous)
        } else {
            path = append_path(&path, &request.paths.private_bin);
            set_registry_value(ENVIRONMENT_KEY, PATH_VALUE, &path)?;
            true
        };
        output.records.push(record(
            RegistrationKind::CliExposure,
            registry_location(ENVIRONMENT_KEY, PATH_VALUE),
            request.paths.private_bin.to_string_lossy().into_owned(),
            owned,
        ));
    } else if present
        && previous
            .record(RegistrationKind::CliExposure)
            .is_some_and(|record| record.owned)
    {
        path = remove_path(&path, &request.paths.private_bin);
        set_registry_value(ENVIRONMENT_KEY, PATH_VALUE, &path)?;
    }
    Ok(())
}

fn cli_entry_is_owned(previous: &RegistrationState) -> bool {
    previous
        .record(RegistrationKind::CliExposure)
        .is_some_and(|record| record.owned)
}

fn path_contains(value: &str, wanted: &Path) -> bool {
    value.split(';').any(|entry| {
        !entry.is_empty()
            && Path::new(entry)
                .to_string_lossy()
                .eq_ignore_ascii_case(&wanted.to_string_lossy())
    })
}

fn append_path(value: &str, entry: &Path) -> String {
    let prefix = value.trim_end_matches(';');
    if prefix.is_empty() {
        entry.to_string_lossy().into_owned()
    } else {
        format!("{prefix};{}", entry.display())
    }
}

fn remove_path(value: &str, unwanted: &Path) -> String {
    value
        .split(';')
        .filter(|entry| {
            !entry.is_empty()
                && !Path::new(entry)
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&unwanted.to_string_lossy())
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(windows)]
fn query_registry_value(key: &str, name: &str) -> Result<Option<String>> {
    let output = Command::new("reg.exe")
        .args(["query", key, "/v", name])
        .output()
        .context("failed to query the per-user Windows registry")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(parse_registry_value(
        &String::from_utf8_lossy(&output.stdout),
        name,
    ))
}

fn parse_registry_value(output: &str, name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some(name) && matches!(fields.next(), Some("REG_SZ" | "REG_EXPAND_SZ")))
            .then(|| fields.collect::<Vec<_>>().join(" "))
    })
}

#[cfg(windows)]
fn set_registry_value(key: &str, name: &str, value: &str) -> Result<()> {
    run_reg(["add", key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])
}

#[cfg(windows)]
fn delete_registry_value(key: &str, name: &str) -> Result<()> {
    if query_registry_value(key, name)?.is_some() {
        run_reg(["delete", key, "/v", name, "/f"])?;
    }
    Ok(())
}

#[cfg(windows)]
fn run_reg<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("reg.exe")
        .args(args)
        .output()
        .context("failed to run the Windows registry tool")?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "Windows registry update failed: {}{}",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

#[cfg(windows)]
fn write_shortcut(path: &Path, target: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("settings shortcut path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create `{}`", parent.display()))?;
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.Save()",
        powershell_path(path),
        powershell_path(target)
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("failed to create the settings Start Menu shortcut")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "failed to create settings shortcut: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

#[cfg(windows)]
fn read_shortcut_target(path: &Path) -> Result<String> {
    let script = format!(
        "(New-Object -ComObject WScript.Shell).CreateShortcut('{}').TargetPath",
        powershell_path(path)
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("failed to inspect the settings Start Menu shortcut")?;
    if !output.status.success() {
        bail!(
            "failed to inspect settings shortcut: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("settings shortcut target is not valid UTF-8")?
        .trim()
        .to_owned())
}

fn powershell_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg(windows)]
fn remove_owned_file(previous: &RegistrationState, kind: RegistrationKind) -> Result<()> {
    if let Some(record) = previous.record(kind).filter(|record| record.owned)
        && record.location.is_file()
    {
        fs::remove_file(&record.location)
            .with_context(|| format!("failed to remove `{}`", record.location.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_run_value_with_spaces() {
        let output = r#"
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run
    gflickAgent    REG_SZ    "C:\Users\Test User\gflick-agent.exe" --background-worker
"#;
        assert_eq!(
            parse_registry_value(output, AGENT_VALUE).as_deref(),
            Some(r#""C:\Users\Test User\gflick-agent.exe" --background-worker"#)
        );
    }

    #[test]
    fn path_edit_preserves_unrelated_entries() {
        let bin = Path::new(r"C:\Users\Test\gflick\bin");
        let original = r"C:\Windows;C:\Tools";
        let added = append_path(original, bin);
        assert!(path_contains(&added, bin));
        assert_eq!(remove_path(&added, bin), original);
    }

    #[test]
    fn startup_commands_are_independent() {
        let paths = PlatformPaths {
            install_root: PathBuf::from(r"C:\gflick"),
            private_bin: PathBuf::from(r"C:\gflick\bin"),
            private_app: PathBuf::new(),
            user_applications: PathBuf::new(),
            user_local_bin: PathBuf::new(),
            state_file: PathBuf::new(),
            tray_ready: PathBuf::new(),
            tray_stop: PathBuf::new(),
            preferences: PathBuf::new(),
            logs: PathBuf::new(),
        };
        assert!(agent_command(&paths).contains("--background-worker"));
        assert!(!agent_command(&paths).contains("gflick-tray"));
        assert_eq!(tray_command(&paths), r#""C:\gflick\bin\gflick-tray.exe""#);
    }

    #[test]
    fn preexisting_registrations_are_never_claimed_or_overwritten() {
        let kind = RegistrationKind::AgentStartup;
        let expected = r#""C:\gflick\gflick-agent.exe" --background-worker"#;
        let exact = RegistrationState {
            records: vec![record(kind, "existing".into(), expected.into(), false)],
            path_warning: None,
        };
        assert_eq!(
            registration_decision(&exact, kind, expected).unwrap(),
            RegistrationDecision::PreserveUnowned
        );
        let different = RegistrationState {
            records: vec![record(
                kind,
                "existing".into(),
                "other command".into(),
                false,
            )],
            path_warning: None,
        };
        assert!(registration_decision(&different, kind, expected).is_err());
        let preexisting_path = RegistrationState {
            records: vec![record(
                RegistrationKind::CliExposure,
                "PATH".into(),
                r"C:\gflick\bin".into(),
                false,
            )],
            path_warning: None,
        };
        assert!(!cli_entry_is_owned(&preexisting_path));
    }

    #[test]
    fn preexisting_settings_shortcut_is_not_eligible_for_overwrite() {
        let existing = RegistrationState {
            records: vec![record(
                RegistrationKind::SettingsLauncher,
                "gflick.lnk".into(),
                "<pre-existing shortcut>".into(),
                false,
            )],
            path_warning: None,
        };
        assert!(
            registration_decision(
                &existing,
                RegistrationKind::SettingsLauncher,
                r"C:\gflick\gflick-settings.exe"
            )
            .is_err()
        );
    }

    #[test]
    fn powershell_paths_escape_single_quotes() {
        assert_eq!(
            powershell_path(Path::new(r"C:\Users\O'Brien\gflick.lnk")),
            r"C:\Users\O''Brien\gflick.lnk"
        );
    }
}
