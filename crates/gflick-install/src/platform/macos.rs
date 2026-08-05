//! Apple Silicon macOS per-user registration implementation.

#[cfg(any(target_os = "macos", test))]
use std::path::Path;

#[cfg(target_os = "macos")]
use std::{fs, path::PathBuf, process::Command};

#[cfg(target_os = "macos")]
use anyhow::{Context, Result, bail};
#[cfg(target_os = "macos")]
use directories::BaseDirs;
#[cfg(target_os = "macos")]
use tempfile::NamedTempFile;

#[cfg(target_os = "macos")]
use super::{
    PlatformBackend, PlatformPaths, RegistrationKind, RegistrationRecord, RegistrationRequest,
    RegistrationState,
};
#[cfg(all(test, not(target_os = "macos")))]
use super::{RegistrationKind, RegistrationRecord, RegistrationState};
#[cfg(target_os = "macos")]
use crate::manifest::Component;

#[cfg(any(target_os = "macos", test))]
const AGENT_LABEL: &str = "io.github.r4ultv.gflick.agent";
#[cfg(any(target_os = "macos", test))]
const TRAY_LABEL: &str = "io.github.r4ultv.gflick.tray";

#[derive(Clone, Debug, Default)]
pub struct MacOsPlatform;

impl MacOsPlatform {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "macos")]
impl PlatformBackend for MacOsPlatform {
    fn paths(&self) -> Result<PlatformPaths> {
        if !cfg!(target_arch = "aarch64") {
            bail!("GFlick supports Apple Silicon macOS only; Intel macOS is unsupported");
        }
        let base =
            BaseDirs::new().context("could not determine the current macOS home directory")?;
        let root = base.home_dir().join("Library/Application Support/gflick");
        Ok(PlatformPaths {
            install_root: root.clone(),
            private_bin: root.join("bin"),
            private_app: root.join("GFlick.app"),
            user_applications: base.home_dir().join("Applications/GFlick.app"),
            user_local_bin: base.home_dir().join(".local/bin"),
            state_file: root.join("install-state.json"),
            tray_ready: root.join("tray.ready"),
            tray_stop: root.join("tray.stop"),
            preferences: root.join("settings.json"),
            logs: base.home_dir().join("Library/Logs/gflick"),
        })
    }

    fn snapshot_registrations(&self) -> Result<RegistrationState> {
        let paths = self.paths()?;
        let mut records = Vec::new();
        for (kind, label) in [
            (RegistrationKind::AgentStartup, AGENT_LABEL),
            (RegistrationKind::TrayStartup, TRAY_LABEL),
        ] {
            let plist = launch_agent_path(label)?;
            if plist.is_file() {
                records.push(RegistrationRecord {
                    kind,
                    location: plist.clone(),
                    value: fs::read_to_string(&plist).with_context(|| {
                        format!("failed to read LaunchAgent `{}`", plist.display())
                    })?,
                    owned: false,
                });
            }
        }

        let link = paths.user_local_bin.join("gflick");
        if let Ok(target) = fs::read_link(&link) {
            records.push(RegistrationRecord {
                kind: RegistrationKind::CliExposure,
                location: link,
                value: target.to_string_lossy().into_owned(),
                owned: false,
            });
        }
        Ok(RegistrationState {
            records,
            path_warning: cli_path_warning(&paths.user_local_bin),
        })
    }

    fn reconcile_registrations(
        &self,
        request: &RegistrationRequest,
        previous: &RegistrationState,
    ) -> Result<RegistrationState> {
        let mut output = RegistrationState::default();
        reconcile_launch_agent(
            Component::Agent,
            RegistrationKind::AgentStartup,
            AGENT_LABEL,
            &request.paths.private_bin.join("gflick-agent"),
            &["--background-worker"],
            request,
            previous,
            &mut output,
        )?;
        reconcile_launch_agent(
            Component::Tray,
            RegistrationKind::TrayStartup,
            TRAY_LABEL,
            &request.paths.private_app.join("Contents/MacOS/gflick-tray"),
            &[],
            request,
            previous,
            &mut output,
        )?;
        reconcile_cli(request, previous, &mut output)?;
        output.path_warning = cli_path_warning(&request.paths.user_local_bin);
        Ok(output)
    }

    fn restore_registrations(&self, snapshot: &RegistrationState) -> Result<()> {
        for (kind, label) in [
            (RegistrationKind::AgentStartup, AGENT_LABEL),
            (RegistrationKind::TrayStartup, TRAY_LABEL),
        ] {
            let plist = launch_agent_path(label)?;
            bootout_if_loaded(label)?;
            if let Some(record) = snapshot.record(kind) {
                write_atomic(&plist, record.value.as_bytes())?;
                bootstrap(&plist)?;
            } else if plist.is_file() {
                fs::remove_file(&plist).with_context(|| {
                    format!("failed to remove LaunchAgent `{}`", plist.display())
                })?;
            }
        }

        let paths = self.paths()?;
        let link = paths.user_local_bin.join("gflick");
        if let Some(record) = snapshot.record(RegistrationKind::CliExposure) {
            replace_symlink(Path::new(&record.value), &link)?;
        } else if link.symlink_metadata().is_ok() {
            fs::remove_file(&link)
                .with_context(|| format!("failed to remove CLI symlink `{}`", link.display()))?;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(target_os = "macos")]
fn reconcile_launch_agent(
    component: Component,
    kind: RegistrationKind,
    label: &str,
    executable: &Path,
    arguments: &[&str],
    request: &RegistrationRequest,
    previous: &RegistrationState,
    output: &mut RegistrationState,
) -> Result<()> {
    let plist_path = launch_agent_path(label)?;
    if request.components.contains(&component) {
        let contents = launch_agent_contents(label, executable, arguments, &request.paths.logs);
        match registration_decision(previous, kind, &contents)? {
            RegistrationDecision::PreserveUnowned => {
                output.records.push(RegistrationRecord {
                    kind,
                    location: plist_path,
                    value: contents,
                    owned: false,
                });
            }
            RegistrationDecision::WriteOwned => {
                bootout_if_loaded(label)?;
                write_atomic(&plist_path, contents.as_bytes())?;
                bootstrap(&plist_path)?;
                output.records.push(RegistrationRecord {
                    kind,
                    location: plist_path,
                    value: contents,
                    owned: true,
                });
            }
        }
    } else if previous.record(kind).is_some_and(|record| record.owned) {
        bootout_if_loaded(label)?;
        if plist_path.is_file() {
            fs::remove_file(&plist_path).with_context(|| {
                format!("failed to remove LaunchAgent `{}`", plist_path.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistrationDecision {
    WriteOwned,
    PreserveUnowned,
}

#[cfg(any(target_os = "macos", test))]
fn registration_decision(
    previous: &RegistrationState,
    kind: RegistrationKind,
    expected: &str,
) -> anyhow::Result<RegistrationDecision> {
    match previous.record(kind) {
        Some(existing) if !existing.owned && existing.value != expected => {
            anyhow::bail!(
                "refusing to overwrite pre-existing registration `{}`",
                existing.location.display()
            )
        }
        Some(existing) if !existing.owned => Ok(RegistrationDecision::PreserveUnowned),
        _ => Ok(RegistrationDecision::WriteOwned),
    }
}

#[cfg(target_os = "macos")]
fn reconcile_cli(
    request: &RegistrationRequest,
    previous: &RegistrationState,
    output: &mut RegistrationState,
) -> Result<()> {
    let link = request.paths.user_local_bin.join("gflick");
    let target = request.paths.private_bin.join("gflick");
    if request.components.contains(&Component::Cli) {
        let owned = match fs::read_link(&link) {
            Ok(existing) if existing == target => previous
                .record(RegistrationKind::CliExposure)
                .is_some_and(|record| record.owned),
            Ok(_) => bail!(
                "refusing to replace unrelated CLI symlink `{}`",
                link.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                replace_symlink(&target, &link)?;
                true
            }
            Err(error) => return Err(error).context("failed to inspect the CLI symlink"),
        };
        output.records.push(RegistrationRecord {
            kind: RegistrationKind::CliExposure,
            location: link,
            value: target.to_string_lossy().into_owned(),
            owned,
        });
    } else if previous
        .record(RegistrationKind::CliExposure)
        .is_some_and(|record| record.owned)
        && link.symlink_metadata().is_ok()
    {
        fs::remove_file(&link)
            .with_context(|| format!("failed to remove CLI symlink `{}`", link.display()))?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn launch_agent_contents(
    label: &str,
    executable: &Path,
    arguments: &[&str],
    logs: &Path,
) -> String {
    let mut program_arguments = format!(
        "    <string>{}</string>\n",
        xml_escape(&executable.to_string_lossy())
    );
    for argument in arguments {
        program_arguments.push_str(&format!("    <string>{}</string>\n", xml_escape(argument)));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{}</string>
  <key>ProgramArguments</key>
  <array>
{}</array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        xml_escape(label),
        program_arguments,
        xml_escape(&logs.join(format!("{label}.log")).to_string_lossy()),
        xml_escape(&logs.join(format!("{label}-error.log")).to_string_lossy())
    )
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn cli_path_warning(local_bin: &Path) -> Option<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .any(|entry| entry == local_bin)
        .then_some(())
        .map_or_else(
            || {
                Some(format!(
                    "`{}` is not on PATH; add it in your shell environment",
                    local_bin.display()
                ))
            },
            |_| None,
        )
}

#[cfg(target_os = "macos")]
fn launch_agent_path(label: &str) -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine the current macOS home directory")?;
    Ok(base
        .home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to determine the current macOS user ID")?;
    if !output.status.success() {
        bail!("`id -u` failed");
    }
    Ok(format!(
        "gui/{}",
        String::from_utf8(output.stdout)
            .context("`id -u` returned non-UTF-8 output")?
            .trim()
    ))
}

#[cfg(target_os = "macos")]
fn bootout_if_loaded(label: &str) -> Result<()> {
    let target = format!("{}/{label}", launchd_domain()?);
    let loaded = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .context("failed to query launchd")?
        .status
        .success();
    if loaded {
        run_launchctl(["bootout", &target])?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn bootstrap(plist: &Path) -> Result<()> {
    let domain = launchd_domain()?;
    run_launchctl(["bootstrap", &domain, &plist.to_string_lossy()])
}

#[cfg(target_os = "macos")]
fn run_launchctl<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("launchctl")
        .args(args)
        .output()
        .context("failed to run launchctl")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "launchctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

#[cfg(target_os = "macos")]
fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path.parent().context("registration path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create `{}`", parent.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in `{}`", parent.display()))?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace `{}`", path.display()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn replace_symlink(target: &Path, link: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    let parent = link.parent().context("CLI symlink has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create `{}`", parent.display()))?;
    if link.symlink_metadata().is_ok() {
        fs::remove_file(link)
            .with_context(|| format!("failed to replace CLI symlink `{}`", link.display()))?;
    }
    symlink(target, link).with_context(|| {
        format!(
            "failed to link `{}` to `{}`",
            link.display(),
            target.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agents_are_independent_and_escape_paths() {
        let logs = Path::new("/Users/a&b/Library/Logs/gflick");
        let agent = launch_agent_contents(
            AGENT_LABEL,
            Path::new("/Users/a&b/gflick-agent"),
            &["--background-worker"],
            logs,
        );
        let tray = launch_agent_contents(
            TRAY_LABEL,
            Path::new("/Users/a&b/GFlick.app/Contents/MacOS/gflick-tray"),
            &[],
            logs,
        );
        assert!(agent.contains("--background-worker"));
        assert!(!agent.contains(TRAY_LABEL));
        assert!(!tray.contains(AGENT_LABEL));
        assert!(agent.contains("/Users/a&amp;b/gflick-agent"));
    }

    #[test]
    fn settings_has_no_startup_registration_kind() {
        assert_ne!(
            RegistrationKind::SettingsLauncher,
            RegistrationKind::AgentStartup
        );
        assert_ne!(
            RegistrationKind::SettingsLauncher,
            RegistrationKind::TrayStartup
        );
    }

    #[test]
    fn preexisting_launch_agent_is_preserved_or_rejected_never_claimed() {
        let kind = RegistrationKind::AgentStartup;
        let expected = "expected plist";
        let exact = RegistrationState {
            records: vec![RegistrationRecord {
                kind,
                location: "/Library/LaunchAgents/existing.plist".into(),
                value: expected.into(),
                owned: false,
            }],
            path_warning: None,
        };
        assert_eq!(
            registration_decision(&exact, kind, expected).unwrap(),
            RegistrationDecision::PreserveUnowned
        );
        let different = RegistrationState {
            records: vec![RegistrationRecord {
                kind,
                location: "/Library/LaunchAgents/existing.plist".into(),
                value: "other plist".into(),
                owned: false,
            }],
            path_warning: None,
        };
        assert!(registration_decision(&different, kind, expected).is_err());
    }
}
