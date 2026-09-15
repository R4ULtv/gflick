//! User-session controls. Only invoked by explicit preference actions.
use anyhow::{Context, Result, bail};
use gflick_protocol::{RequestCommand, ResponseData};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn root() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("No user data directory")?
        .data_local_dir()
        .join("gflick"))
}
pub fn executable(name: &str) -> Result<PathBuf> {
    let filename = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let current = std::env::current_exe()?;
    for path in [
        current
            .parent()
            .context("No executable directory")?
            .join(&filename),
        root()?.join("bin").join(&filename),
    ] {
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("{name} is not installed beside the settings app. Build or install that component first.")
}
fn spawn(name: &str) -> Result<std::process::Child> {
    let mut command = Command::new(executable(name)?);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
        if name == "gflick-agent" {
            command.arg("--background-worker");
        }
    }
    command
        .spawn()
        .with_context(|| format!("Could not start {name}"))
}
pub fn ping() -> bool {
    matches!(
        gflick_client::request(RequestCommand::Ping),
        Ok(ResponseData::Pong)
    )
}
pub fn restart() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("{}/{LABEL}", launch_domain()?);
        if Command::new("launchctl")
            .args(["print", &target])
            .output()?
            .status
            .success()
        {
            let output = Command::new("launchctl")
                .args(["kickstart", "-k", &target])
                .output()?;
            if !output.status.success() {
                bail!(
                    "Could not restart the login agent: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if ping() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }
            bail!("The login agent did not respond after restarting");
        }
    }
    // Resolve before stopping the current process so a missing binary leaves it running.
    executable("gflick-agent")?;
    if ping() {
        gflick_client::request(RequestCommand::Shutdown)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while ping() {
            if Instant::now() >= deadline {
                bail!("The agent did not stop. It may be managed by a system service.");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    let mut child = spawn("gflick-agent")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if ping() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!("Agent exited before connecting: {status}");
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!("Agent started but did not respond in time. Try Refresh.")
}
fn tray_generation() -> Result<Option<String>> {
    let token = match fs::read_to_string(root()?.join("tray.ready")) {
        Ok(token) => token,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let Some(pid) = token
        .split('-')
        .next()
        .and_then(|s| s.parse::<usize>().ok())
    else {
        return Ok(None);
    };
    let pid = sysinfo::Pid::from(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    Ok(system
        .process(pid)
        .filter(|p| p.name().to_string_lossy().contains("gflick-tray"))
        .map(|_| token))
}

pub fn tray_status() -> Result<bool> {
    Ok(startup_status_for("gflick-tray")? || tray_generation()?.is_some())
}

pub fn set_tray(enabled: bool) -> Result<()> {
    let previous = startup_status_for("gflick-tray")?;
    set_startup_for("gflick-tray", enabled)?;
    if enabled {
        if let Err(error) = ensure_tray() {
            if let Err(rollback) = set_startup_for("gflick-tray", previous) {
                bail!("{error:#}; could not restore tray startup: {rollback:#}");
            }
            return Err(error);
        }
    } else if let Some(token) = tray_generation()? {
        // The tray's generation-scoped shutdown handshake never contacts the agent.
        let path = root()?.join("tray.stop");
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        use std::io::Write;
        file.write_all(token.as_bytes())?;
        file.persist(path)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while tray_generation()?.is_some() {
            if Instant::now() >= deadline {
                bail!("Tray startup is disabled, but the running tray did not stop in time");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

pub fn startup_status() -> Result<bool> {
    startup_status_for("gflick-agent")
}
pub fn set_startup(enabled: bool) -> Result<()> {
    set_startup_for("gflick-agent", enabled)
}

pub fn ensure_tray() -> Result<()> {
    if tray_generation()?.is_some() {
        return Ok(());
    }
    let mut child = spawn("gflick-tray")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if tray_generation()?.is_some() {
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            bail!("Tray exited before becoming ready");
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!("Tray did not become ready in time")
}

#[cfg(target_os = "macos")]
const LABEL: &str = "io.github.r4ultv.gflick.agent";
#[cfg(target_os = "macos")]
fn launch_label(name: &str) -> String {
    format!(
        "io.github.r4ultv.gflick.{}",
        name.strip_prefix("gflick-").unwrap_or(name)
    )
}
#[cfg(target_os = "macos")]
fn launch_path(name: &str) -> Result<PathBuf> {
    let label = launch_label(name);
    Ok(directories::BaseDirs::new()
        .context("No home directory")?
        .home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}
#[cfg(target_os = "macos")]
fn launch_domain() -> Result<String> {
    let output = Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        bail!("Cannot determine login session");
    }
    Ok(format!("gui/{}", String::from_utf8(output.stdout)?.trim()))
}
/// Reads one label from `launchctl print-disabled`, accepting old and new spellings.
#[cfg(target_os = "macos")]
fn is_disabled(text: &str, label: &str) -> bool {
    text.lines()
        .filter(|line| line.contains(label))
        .any(|line| {
            matches!(
                line.rsplit("=>").next().unwrap_or_default().trim(),
                "true" | "disabled"
            )
        })
}
#[cfg(target_os = "macos")]
fn startup_status_for(name: &str) -> Result<bool> {
    let label = launch_label(name);
    if !launch_path(name)?.is_file() {
        return Ok(false);
    }
    let output = Command::new("launchctl")
        .args(["print-disabled", &launch_domain()?])
        .output()?;
    if !output.status.success() {
        bail!("Cannot read login-item status");
    }
    Ok(!is_disabled(&String::from_utf8(output.stdout)?, &label))
}
#[cfg(target_os = "macos")]
fn set_startup_for(name: &str, enabled: bool) -> Result<()> {
    let label = launch_label(name);
    let path = launch_path(name)?;
    let created = enabled && !path.exists();
    if created {
        let executable = executable(name)?;
        fs::create_dir_all(path.parent().unwrap())?;
        let mut dict = plist::Dictionary::new();
        dict.insert("Label".into(), label.clone().into());
        dict.insert(
            "ProgramArguments".into(),
            plist::Value::Array(vec![executable.to_string_lossy().into_owned().into()]),
        );
        dict.insert("RunAtLoad".into(), true.into());
        plist::Value::Dictionary(dict).to_file_xml(&path)?;
    }
    let result = Command::new("launchctl")
        .args([
            if enabled { "enable" } else { "disable" },
            &format!("{}/{label}", launch_domain()?),
        ])
        .output()?;
    if !result.status.success() {
        if created {
            let _ = fs::remove_file(path);
        }
        bail!(
            "Could not update login item: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(())
}
#[cfg(windows)]
fn registry_name(name: &str) -> &str {
    if name == "gflick-agent" {
        "GFlickAgent"
    } else {
        "GFlickTray"
    }
}
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
fn startup_status_for(name: &str) -> Result<bool> {
    Ok(Command::new("reg")
        .args(["query", RUN_KEY, "/v", registry_name(name)])
        .output()?
        .status
        .success())
}
#[cfg(windows)]
fn set_startup_for(name: &str, enabled: bool) -> Result<()> {
    let mut cmd = Command::new("reg");
    if enabled {
        cmd.args([
            "add",
            RUN_KEY,
            "/v",
            registry_name(name),
            "/t",
            "REG_SZ",
            "/d",
        ])
        .arg(format!(
            "\"{}\"{}",
            executable(name)?.display(),
            if name == "gflick-agent" {
                " --background-worker"
            } else {
                ""
            }
        ))
        .arg("/f");
    } else {
        if !startup_status_for(name)? {
            return Ok(());
        }
        cmd.args(["delete", RUN_KEY, "/v", registry_name(name), "/f"]);
    }
    let result = cmd.output()?;
    if !result.status.success() {
        bail!(
            "Could not update login item: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(())
}
#[cfg(not(any(target_os = "macos", windows)))]
fn startup_status_for(_name: &str) -> Result<bool> {
    bail!("Login items are supported on macOS and Windows")
}
#[cfg(not(any(target_os = "macos", windows)))]
fn set_startup_for(_name: &str, _: bool) -> Result<()> {
    bail!("Login items are supported on macOS and Windows")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn reads_both_spellings_of_a_disabled_login_item() {
        let modern = "\tdisabled services = {\n\t\t\"io.github.r4ultv.gflick.agent\" => disabled\n\t\t\"io.github.r4ultv.gflick.tray\" => enabled\n\t}";
        assert!(is_disabled(modern, "io.github.r4ultv.gflick.agent"));
        assert!(!is_disabled(modern, "io.github.r4ultv.gflick.tray"));

        let legacy = "\t\t\"io.github.r4ultv.gflick.agent\" => true\n\t\t\"io.github.r4ultv.gflick.tray\" => false";
        assert!(is_disabled(legacy, "io.github.r4ultv.gflick.agent"));
        assert!(!is_disabled(legacy, "io.github.r4ultv.gflick.tray"));

        // A label the list does not mention has no override, so it is enabled.
        assert!(!is_disabled(modern, "io.github.r4ultv.gflick.missing"));
    }
}
