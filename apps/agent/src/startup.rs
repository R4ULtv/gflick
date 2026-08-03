use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use tempfile::NamedTempFile;

const APP_DIRECTORY: &str = "open-hub";
#[cfg(any(target_os = "macos", test))]
const MACOS_LABEL: &str = "io.github.r4ultv.open-hub.agent";
#[cfg(any(target_os = "macos", test))]
const MACOS_TRAY_ICON: &[u8] = include_bytes!("../../../assets/favicon.icns");

#[derive(Debug)]
pub struct StartupStatus {
    pub executable: PathBuf,
    pub executable_exists: bool,
    pub tray_executable: PathBuf,
    pub tray_executable_exists: bool,
    pub registration: PathBuf,
    pub registered: bool,
    pub running: Option<bool>,
}

pub fn install() -> Result<StartupStatus> {
    platform::install()
}

pub fn uninstall() -> Result<StartupStatus> {
    platform::uninstall()
}

pub fn status() -> Result<StartupStatus> {
    platform::status()
}

#[cfg(windows)]
pub fn launch_background() -> Result<()> {
    platform::launch_background()
}

#[cfg(target_os = "macos")]
pub fn launch_tray() -> Result<()> {
    platform::launch_tray()
}

#[cfg(windows)]
pub fn take_stop_request() -> Result<bool> {
    let path = stop_request_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("failed to consume stop request `{}`", path.display())),
    }
}

pub fn print_status(action: &str, status: &StartupStatus) {
    println!("Per-user startup {action}.");
    println!("Executable: {}", status.executable.display());
    println!("Executable present: {}", status.executable_exists);
    println!("Tray executable: {}", status.tray_executable.display());
    println!("Tray executable present: {}", status.tray_executable_exists);
    println!("Registration: {}", status.registration.display());
    println!("Registered: {}", status.registered);
    if let Some(running) = status.running {
        println!("Loaded by launchd: {running}");
    }
}

fn installed_executable_path() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine the per-user data directory")?;
    let filename = if cfg!(windows) {
        "open-hub-agent.exe"
    } else {
        "open-hub-agent"
    };
    Ok(base
        .data_local_dir()
        .join(APP_DIRECTORY)
        .join("bin")
        .join(filename))
}

fn installed_tray_executable_path() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine the per-user data directory")?;
    let root = base.data_local_dir().join(APP_DIRECTORY);
    if cfg!(target_os = "macos") {
        Ok(root.join("Open Hub.app/Contents/MacOS/open-hub-tray"))
    } else {
        Ok(root.join("bin/open-hub-tray.exe"))
    }
}

#[cfg(target_os = "macos")]
fn installed_tray_bundle_path() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine the per-user data directory")?;
    Ok(base
        .data_local_dir()
        .join(APP_DIRECTORY)
        .join("Open Hub.app"))
}

#[cfg(windows)]
fn stop_request_path() -> Result<PathBuf> {
    let base = BaseDirs::new().context("could not determine the per-user data directory")?;
    Ok(base.data_local_dir().join(APP_DIRECTORY).join("agent.stop"))
}

fn install_current_executable(target: &Path) -> Result<()> {
    let source = std::env::current_exe().context("could not locate the running agent binary")?;
    install_executable(&source, target, "agent")
}

fn install_sibling_tray(target: &Path) -> Result<()> {
    let agent = std::env::current_exe().context("could not locate the running agent binary")?;
    let filename = if cfg!(windows) {
        "open-hub-tray.exe"
    } else {
        "open-hub-tray"
    };
    let source = agent
        .parent()
        .context("running agent path has no parent directory")?
        .join(filename);
    if !source.is_file() {
        anyhow::bail!(
            "the tray executable was not found at `{}`; build both release binaries before installing startup",
            source.display()
        );
    }
    install_executable(&source, target, "tray")
}

fn install_executable(source: &Path, target: &Path, component: &str) -> Result<()> {
    if same_path(source, target) {
        return Ok(());
    }

    let parent = target
        .parent()
        .context("installed agent path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create {component} installation directory `{}`",
            parent.display()
        )
    })?;
    let temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary binary in `{}`",
            parent.display()
        )
    })?;
    fs::copy(source, temporary.path()).with_context(|| {
        format!(
            "failed to copy {component} from `{}` to `{}`",
            source.display(),
            temporary.path().display()
        )
    })?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to flush the installed {component} binary"))?;
    temporary
        .persist(target)
        .map_err(|error| error.error)
        .with_context(|| {
            format!(
                "failed to replace installed {component} `{}`",
                target.display()
            )
        })?;
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_plist_contents(executable: &Path, stdout: &Path, stderr: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{MACOS_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>--launch-tray</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        xml_escape(&executable.to_string_lossy()),
        xml_escape(&stdout.to_string_lossy()),
        xml_escape(&stderr.to_string_lossy())
    )
}

#[cfg(any(target_os = "macos", test))]
fn macos_tray_info_plist_contents() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key>
  <string>Open Hub</string>
  <key>CFBundleExecutable</key>
  <string>open-hub-tray</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.r4ultv.open-hub</string>
  <key>CFBundleIconFile</key>
  <string>favicon.icns</string>
  <key>CFBundleName</key>
  <string>Open Hub</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
"#
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_plist_escapes_paths_and_uses_background_policy() {
        let plist = macos_plist_contents(
            Path::new("/Users/a&b/open-hub-agent"),
            Path::new("/tmp/out<1>.log"),
            Path::new("/tmp/error.log"),
        );
        assert!(plist.contains("/Users/a&amp;b/open-hub-agent"));
        assert!(plist.contains("/tmp/out&lt;1&gt;.log"));
        assert!(plist.contains("<string>Background</string>"));
        assert!(plist.contains("<string>--launch-tray</string>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
    }

    #[test]
    fn macos_tray_bundle_uses_the_supplied_icon_and_background_policy() {
        let plist = macos_tray_info_plist_contents();
        assert!(plist.contains("<string>favicon.icns</string>"));
        assert!(plist.contains("<key>LSUIElement</key>\n  <true/>"));
        assert_eq!(&MACOS_TRAY_ICON[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(MACOS_TRAY_ICON[4..8].try_into().unwrap()) as usize,
            MACOS_TRAY_ICON.len()
        );
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        ffi::OsStr,
        fs::{self, OpenOptions},
        io::Write,
        path::PathBuf,
        process::{Command, Stdio},
        thread,
        time::Duration,
    };

    use anyhow::{Context, Result, bail};
    use std::os::windows::process::CommandExt;

    use super::{
        StartupStatus, install_current_executable, install_sibling_tray, installed_executable_path,
        installed_tray_executable_path, same_path, stop_request_path,
    };

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "OpenHubAgent";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn install() -> Result<StartupStatus> {
        let executable = installed_executable_path()?;
        let tray_executable = installed_tray_executable_path()?;
        request_stop(&executable)?;
        wait_for_executable_release(&tray_executable, "tray")?;
        install_current_executable(&executable)?;
        install_sibling_tray(&tray_executable)?;
        let command_line = startup_command(&executable);
        run_reg([
            "add",
            RUN_KEY,
            "/v",
            VALUE_NAME,
            "/t",
            "REG_SZ",
            "/d",
            &command_line,
            "/f",
        ])?;
        let status = status()?;
        spawn_session(&executable, &tray_executable)?;
        Ok(status)
    }

    pub fn uninstall() -> Result<StartupStatus> {
        if registration_exists()? {
            run_reg(["delete", RUN_KEY, "/v", VALUE_NAME, "/f"])?;
        }

        let executable = installed_executable_path()?;
        let tray_executable = installed_tray_executable_path()?;
        let current =
            std::env::current_exe().context("could not locate the running agent binary")?;
        if !same_path(&current, &executable) {
            if executable.exists() {
                request_stop(&executable)?;
            }
            wait_for_executable_release(&tray_executable, "tray")?;
            if executable.exists() {
                std::fs::remove_file(&executable).with_context(|| {
                    format!(
                        "failed to remove installed agent `{}`",
                        executable.display()
                    )
                })?;
            }
            if tray_executable.exists() {
                std::fs::remove_file(&tray_executable).with_context(|| {
                    format!(
                        "failed to remove installed tray `{}`",
                        tray_executable.display()
                    )
                })?;
            }
        }
        status()
    }

    pub fn status() -> Result<StartupStatus> {
        let executable = installed_executable_path()?;
        let tray_executable = installed_tray_executable_path()?;
        let registration = PathBuf::from(format!(r"{RUN_KEY}\{VALUE_NAME}"));
        let registered = query_registration()?
            .is_some_and(|value| value.eq_ignore_ascii_case(&startup_command(&executable)));
        Ok(StartupStatus {
            executable_exists: executable.is_file(),
            executable,
            tray_executable_exists: tray_executable.is_file(),
            tray_executable,
            registration,
            registered,
            running: None,
        })
    }

    pub fn launch_background() -> Result<()> {
        let executable = std::env::current_exe().context("could not locate the agent binary")?;
        let tray_executable = sibling_tray_path(&executable)?;
        spawn_session(&executable, &tray_executable)
    }

    fn spawn_session(
        executable: &std::path::Path,
        tray_executable: &std::path::Path,
    ) -> Result<()> {
        if !tray_executable.is_file() {
            bail!(
                "tray executable `{}` does not exist",
                tray_executable.display()
            );
        }
        let mut agent = Command::new(executable)
            .arg("--background-worker")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to launch background agent `{}`",
                    executable.display()
                )
            })?;
        if let Err(error) = Command::new(tray_executable)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            let _ = agent.kill();
            return Err(error)
                .with_context(|| format!("failed to launch tray `{}`", tray_executable.display()));
        }
        Ok(())
    }

    fn sibling_tray_path(agent: &std::path::Path) -> Result<PathBuf> {
        Ok(agent
            .parent()
            .context("agent executable path has no parent")?
            .join("open-hub-tray.exe"))
    }

    fn request_stop(executable: &std::path::Path) -> Result<()> {
        let executable_exists = executable.exists();
        let stop = stop_request_path()?;
        let parent = stop.parent().context("stop-request path has no parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create agent data directory `{}`",
                parent.display()
            )
        })?;
        let mut file = fs::File::create(&stop)
            .with_context(|| format!("failed to create stop request `{}`", stop.display()))?;
        file.write_all(b"stop\n")
            .context("failed to write agent stop request")?;
        file.sync_all()
            .context("failed to flush agent stop request")?;

        if !executable_exists {
            // A development build may own IPC before the first installation. Give it
            // one normal loop interval to consume the same graceful-stop request.
            for _ in 0..20 {
                if !stop.exists() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = fs::remove_file(&stop);
            return Ok(());
        }

        let mut last_error = None;
        for _ in 0..100 {
            match OpenOptions::new().write(true).open(executable) {
                Ok(_) => {
                    let _ = fs::remove_file(&stop);
                    return Ok(());
                }
                Err(error) if is_executable_lock(&error) => {
                    last_error = Some(error);
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    let _ = fs::remove_file(&stop);
                    return Err(error).with_context(|| {
                        format!(
                            "failed while waiting for agent `{}` to stop",
                            executable.display()
                        )
                    });
                }
            }
        }
        let _ = fs::remove_file(&stop);
        Err(last_error.context("installed agent did not release its executable")?).with_context(
            || {
                format!(
                    "timed out waiting for installed agent `{}` to stop gracefully",
                    executable.display()
                )
            },
        )
    }

    fn wait_for_executable_release(executable: &std::path::Path, component: &str) -> Result<()> {
        if !executable.exists() {
            return Ok(());
        }
        let mut last_error = None;
        for _ in 0..100 {
            match OpenOptions::new().write(true).open(executable) {
                Ok(_) => return Ok(()),
                Err(error) if is_executable_lock(&error) => {
                    last_error = Some(error);
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed while waiting for {component} `{}` to stop",
                            executable.display()
                        )
                    });
                }
            }
        }
        Err(last_error.context("installed executable did not become writable")?).with_context(
            || {
                format!(
                    "timed out waiting for installed {component} `{}` to stop gracefully",
                    executable.display()
                )
            },
        )
    }

    fn is_executable_lock(error: &std::io::Error) -> bool {
        error.kind() == std::io::ErrorKind::PermissionDenied
            || matches!(error.raw_os_error(), Some(5 | 32 | 33))
    }

    fn startup_command(executable: &std::path::Path) -> String {
        format!(r#""{}" --launch-background"#, executable.display())
    }

    fn registration_exists() -> Result<bool> {
        Ok(query_registration()?.is_some())
    }

    fn query_registration() -> Result<Option<String>> {
        let output = Command::new("reg.exe")
            .args(["query", RUN_KEY, "/v", VALUE_NAME])
            .output()
            .context("failed to query the per-user Windows startup registry")?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(parse_registry_value(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn parse_registry_value(output: &str) -> Option<String> {
        output.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some(VALUE_NAME) && fields.next() == Some("REG_SZ"))
                .then(|| fields.collect::<Vec<_>>().join(" "))
        })
    }

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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Windows registry update failed: {}{}",
            stdout.trim(),
            stderr.trim()
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_quoted_startup_command_with_spaces() {
            let output = r#"
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run
    OpenHubAgent    REG_SZ    "C:\Users\Test User\open-hub-agent.exe" --launch-background
"#;
            assert_eq!(
                parse_registry_value(output).as_deref(),
                Some(r#""C:\Users\Test User\open-hub-agent.exe" --launch-background"#)
            );
        }

        #[test]
        fn retries_windows_sharing_violations() {
            assert!(is_executable_lock(&std::io::Error::from_raw_os_error(32)));
            assert!(is_executable_lock(&std::io::Error::from_raw_os_error(33)));
            assert!(!is_executable_lock(&std::io::Error::from_raw_os_error(2)));
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    use anyhow::{Context, Result, bail};
    use directories::BaseDirs;
    use tempfile::NamedTempFile;

    use super::{
        APP_DIRECTORY, MACOS_LABEL, MACOS_TRAY_ICON, StartupStatus, install_current_executable,
        install_sibling_tray, installed_executable_path, installed_tray_bundle_path,
        installed_tray_executable_path, macos_plist_contents, macos_tray_info_plist_contents,
    };

    const PLIST_NAME: &str = "io.github.r4ultv.open-hub.agent.plist";

    pub fn install() -> Result<StartupStatus> {
        let executable = installed_executable_path()?;
        let tray_executable = installed_tray_executable_path()?;
        let paths = launchd_paths()?;
        let target = service_target()?;
        if launchd_loaded(&target)? {
            run_launchctl(["bootout", target.as_str()])?;
        }
        install_current_executable(&executable)?;
        install_sibling_tray(&tray_executable)?;
        write_tray_bundle_metadata(&tray_executable)?;
        fs::create_dir_all(&paths.logs).with_context(|| {
            format!("failed to create log directory `{}`", paths.logs.display())
        })?;
        write_plist(&paths.plist, &executable, &paths.logs)?;

        run_launchctl(["enable", target.as_str()])?;
        let domain = launchd_domain()?;
        run_launchctl([
            "bootstrap",
            domain.as_str(),
            paths.plist.to_string_lossy().as_ref(),
        ])?;
        status()
    }

    pub fn uninstall() -> Result<StartupStatus> {
        let paths = launchd_paths()?;
        let target = service_target()?;
        if launchd_loaded(&target)? {
            run_launchctl(["bootout", target.as_str()])?;
        }
        if paths.plist.exists() {
            fs::remove_file(&paths.plist).with_context(|| {
                format!("failed to remove LaunchAgent `{}`", paths.plist.display())
            })?;
        }
        let executable = installed_executable_path()?;
        let tray_bundle = installed_tray_bundle_path()?;
        if executable.exists() {
            fs::remove_file(&executable).with_context(|| {
                format!(
                    "failed to remove installed agent `{}`",
                    executable.display()
                )
            })?;
        }
        if tray_bundle.exists() {
            fs::remove_dir_all(&tray_bundle).with_context(|| {
                format!(
                    "failed to remove installed tray bundle `{}`",
                    tray_bundle.display()
                )
            })?;
        }
        status()
    }

    pub fn status() -> Result<StartupStatus> {
        let executable = installed_executable_path()?;
        let tray_executable = installed_tray_executable_path()?;
        let paths = launchd_paths()?;
        Ok(StartupStatus {
            executable_exists: executable.is_file(),
            executable,
            tray_executable_exists: tray_executable.is_file(),
            tray_executable,
            registered: paths.plist.is_file(),
            registration: paths.plist,
            running: Some(launchd_loaded(&service_target()?)?),
        })
    }

    pub fn launch_tray() -> Result<()> {
        let tray = installed_tray_executable_path()?;
        Command::new(&tray)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to launch tray `{}`", tray.display()))?;
        Ok(())
    }

    fn write_tray_bundle_metadata(tray_executable: &Path) -> Result<()> {
        let contents = tray_executable
            .parent()
            .and_then(Path::parent)
            .context("installed tray is not inside an app bundle")?;
        let resources = contents.join("Resources");
        fs::create_dir_all(&resources).with_context(|| {
            format!(
                "failed to create tray resource directory `{}`",
                resources.display()
            )
        })?;
        write_atomic(
            &contents.join("Info.plist"),
            macos_tray_info_plist_contents().as_bytes(),
        )?;
        write_atomic(&resources.join("favicon.icns"), MACOS_TRAY_ICON)?;
        Ok(())
    }

    struct LaunchdPaths {
        plist: PathBuf,
        logs: PathBuf,
    }

    fn launchd_paths() -> Result<LaunchdPaths> {
        let base = BaseDirs::new().context("could not determine the user home directory")?;
        Ok(LaunchdPaths {
            plist: base
                .home_dir()
                .join("Library/LaunchAgents")
                .join(PLIST_NAME),
            logs: base.home_dir().join("Library/Logs").join(APP_DIRECTORY),
        })
    }

    fn write_plist(path: &Path, executable: &Path, logs: &Path) -> Result<()> {
        let parent = path.parent().context("LaunchAgent path has no parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create LaunchAgents directory `{}`",
                parent.display()
            )
        })?;
        let stdout = logs.join("agent.log");
        let stderr = logs.join("agent-error.log");
        let contents = macos_plist_contents(executable, &stdout, &stderr);
        write_atomic(path, contents.as_bytes())
            .with_context(|| format!("failed to write LaunchAgent `{}`", path.display()))
    }

    fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
        let parent = path.parent().context("installed file path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory `{}`", parent.display()))?;
        let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
            format!("failed to create temporary file in `{}`", parent.display())
        })?;
        temporary
            .write_all(contents)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
        temporary
            .as_file()
            .sync_all()
            .with_context(|| format!("failed to flush `{}`", path.display()))?;
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to replace `{}`", path.display()))?;
        Ok(())
    }

    fn user_id() -> Result<String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .context("failed to determine the current macOS user ID")?;
        if !output.status.success() {
            bail!("`id -u` failed");
        }
        Ok(String::from_utf8(output.stdout)
            .context("`id -u` returned non-UTF-8 output")?
            .trim()
            .to_owned())
    }

    fn launchd_domain() -> Result<String> {
        Ok(format!("gui/{}", user_id()?))
    }

    fn service_target() -> Result<String> {
        Ok(format!("{}/{MACOS_LABEL}", launchd_domain()?))
    }

    fn launchd_loaded(target: &str) -> Result<bool> {
        Ok(Command::new("launchctl")
            .args(["print", target])
            .output()
            .context("failed to query launchd")?
            .status
            .success())
    }

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
            return Ok(());
        }
        bail!(
            "launchctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}
