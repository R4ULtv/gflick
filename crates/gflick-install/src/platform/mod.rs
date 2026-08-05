//! Per-user installation paths and operating-system registration.
//!
//! The transaction engine depends on [`PlatformBackend`] rather than invoking
//! `reg.exe`, `launchctl`, or a shell directly. Tests can therefore exercise
//! every transition without reading or changing the real user profile.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::manifest::{Component, InstallRoot};

pub mod macos;
#[cfg(windows)]
pub mod windows;

/// Concrete directories selected by a trusted platform implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformPaths {
    pub install_root: PathBuf,
    pub private_bin: PathBuf,
    pub private_app: PathBuf,
    pub user_applications: PathBuf,
    pub user_local_bin: PathBuf,
    pub state_file: PathBuf,
    pub tray_ready: PathBuf,
    pub tray_stop: PathBuf,
    pub preferences: PathBuf,
    pub logs: PathBuf,
}

impl PlatformPaths {
    /// Resolves a closed manifest root to a trusted absolute platform path.
    #[must_use]
    pub fn resolve(&self, root: InstallRoot) -> &std::path::Path {
        match root {
            InstallRoot::PrivateBin => &self.private_bin,
            InstallRoot::PrivateApp => &self.private_app,
            InstallRoot::UserApplications => &self.user_applications,
            InstallRoot::UserLocalBin => &self.user_local_bin,
        }
    }

    #[must_use]
    pub fn allowed_roots(&self) -> Vec<PathBuf> {
        vec![
            self.private_bin.clone(),
            self.private_app.clone(),
            self.user_applications.clone(),
            self.user_local_bin.clone(),
        ]
    }
}

/// A setup-owned integration with the current user's session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationKind {
    AgentStartup,
    TrayStartup,
    SettingsLauncher,
    CliExposure,
}

/// Persisted ownership information used to make removal conservative.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegistrationRecord {
    pub kind: RegistrationKind,
    /// Registry value, plist, shortcut, PATH entry, or symlink managed by setup.
    pub location: PathBuf,
    /// The exact command/target/value setup expects at `location`.
    pub value: String,
    /// False when the integration predated setup and must never be removed.
    pub owned: bool,
}

/// Serializable registration snapshot stored in the installed-state document.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegistrationState {
    #[serde(default)]
    pub records: Vec<RegistrationRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_warning: Option<String>,
}

impl RegistrationState {
    #[must_use]
    pub fn record(&self, kind: RegistrationKind) -> Option<&RegistrationRecord> {
        self.records.iter().find(|record| record.kind == kind)
    }
}

/// Desired platform integrations for one complete selected component set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationRequest {
    pub components: BTreeSet<Component>,
    pub paths: PlatformPaths,
}

/// Injectable boundary for all registry, launchd, shortcut, PATH, and symlink
/// operations. Implementations must treat `previous` as ownership evidence.
pub trait PlatformBackend {
    fn paths(&self) -> Result<PlatformPaths>;

    fn snapshot_registrations(&self) -> Result<RegistrationState>;

    fn reconcile_registrations(
        &self,
        request: &RegistrationRequest,
        previous: &RegistrationState,
    ) -> Result<RegistrationState>;

    fn restore_registrations(&self, snapshot: &RegistrationState) -> Result<()>;

    /// An offline agent is normal and must return `Ok(())`.
    fn request_agent_shutdown(&self) -> Result<()> {
        // A connection failure means the agent is already offline. If it is
        // available, the typed request gives it a chance to release HID and
        // lighting ownership before executable replacement.
        let _ = gflick_client::request(gflick_protocol::RequestCommand::Shutdown);
        Ok(())
    }

    /// Requests a graceful tray exit through a bounded per-user file
    /// handshake. An absent tray is normal and leaves no stale request.
    fn request_tray_shutdown(&self) -> Result<()> {
        let paths = self.paths()?;
        request_tray_shutdown_at(
            &paths.tray_ready,
            &paths.tray_stop,
            40,
            Duration::from_millis(50),
        )
    }
}

fn request_tray_shutdown_at(
    ready_path: &Path,
    stop_path: &Path,
    attempts: usize,
    delay: Duration,
) -> Result<()> {
    match fs::remove_file(stop_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to clear stale tray stop request `{}`",
                    stop_path.display()
                )
            });
        }
    }
    let Some(generation) = read_handshake_token(ready_path)? else {
        return Ok(());
    };
    let parent = stop_path
        .parent()
        .context("tray stop-request path has no parent")?;
    if !parent.is_dir() {
        return Ok(());
    }
    let mut request = fs::File::create(stop_path).with_context(|| {
        format!(
            "failed to create tray stop request `{}`",
            stop_path.display()
        )
    })?;
    request.write_all(generation.as_bytes())?;
    request.write_all(b"\n")?;
    request.sync_all()?;
    drop(request);

    for _ in 0..attempts {
        match read_handshake_token(ready_path)? {
            None => {
                remove_matching_request(stop_path, &generation)?;
                return Ok(());
            }
            Some(current) if current != generation => {
                remove_matching_request(stop_path, &generation)?;
                return Ok(());
            }
            Some(_) => {}
        }
        if !stop_path.exists() {
            thread::sleep(delay);
            continue;
        }
        thread::sleep(delay);
    }
    remove_matching_request(stop_path, &generation)
}

fn read_handshake_token(path: &Path) -> Result<Option<String>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read tray handshake `{}`", path.display()));
        }
    };
    let token = contents.trim();
    if token.is_empty()
        || token.len() > 128
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Ok(None);
    }
    Ok(Some(token.to_owned()))
}

fn remove_matching_request(path: &Path, generation: &str) -> Result<()> {
    if read_handshake_token(path)?.as_deref() != Some(generation) {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to clean tray stop request `{}`", path.display())),
    }
}

#[cfg(windows)]
pub type NativePlatform = windows::WindowsPlatform;

#[cfg(target_os = "macos")]
pub type NativePlatform = macos::MacOsPlatform;

#[cfg(not(any(windows, target_os = "macos")))]
compile_error!("gflick-install supports only Windows and Apple Silicon macOS");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_install_roots_resolve_only_inside_platform_paths() {
        let paths = PlatformPaths {
            install_root: PathBuf::from("/profile/gflick"),
            private_bin: PathBuf::from("/profile/gflick/bin"),
            private_app: PathBuf::from("/profile/gflick/app"),
            user_applications: PathBuf::from("/profile/Applications"),
            user_local_bin: PathBuf::from("/profile/.local/bin"),
            state_file: PathBuf::from("/profile/gflick/install-state.json"),
            tray_ready: PathBuf::from("/profile/gflick/tray.ready"),
            tray_stop: PathBuf::from("/profile/gflick/tray.stop"),
            preferences: PathBuf::from("/profile/gflick/settings.json"),
            logs: PathBuf::from("/profile/gflick/logs"),
        };

        assert_eq!(
            paths.resolve(InstallRoot::PrivateBin),
            std::path::Path::new("/profile/gflick/bin")
        );
        assert_eq!(
            paths.resolve(InstallRoot::UserApplications),
            std::path::Path::new("/profile/Applications")
        );
    }

    #[test]
    fn absent_tray_is_success_and_request_is_not_left_stale() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("tray.ready");
        let stop = directory.path().join("tray.stop");
        fs::write(&stop, b"old-generation\n").unwrap();
        request_tray_shutdown_at(&ready, &stop, 1, Duration::ZERO).unwrap();
        assert!(!ready.exists());
        assert!(!stop.exists());
    }

    #[test]
    fn running_tray_consumes_the_bounded_request() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("tray.ready");
        let stop = directory.path().join("tray.stop");
        fs::write(&ready, b"live-generation\n").unwrap();
        let watcher_ready = ready.clone();
        let watcher_stop = stop.clone();
        let watcher = thread::spawn(move || {
            for _ in 0..100 {
                if read_handshake_token(&watcher_stop).unwrap().as_deref()
                    == Some("live-generation")
                {
                    fs::remove_file(&watcher_stop).unwrap();
                    fs::remove_file(&watcher_ready).unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(2));
            }
            panic!("stop request was not observed");
        });
        request_tray_shutdown_at(&ready, &stop, 100, Duration::from_millis(2)).unwrap();
        watcher.join().unwrap();
        assert!(!ready.exists());
        assert!(!stop.exists());
    }

    #[test]
    fn mismatched_request_is_cleaned_without_disarming_live_generation() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("tray.ready");
        let stop = directory.path().join("tray.stop");
        fs::write(&ready, b"new-generation\n").unwrap();
        fs::write(&stop, b"old-generation\n").unwrap();
        remove_matching_request(&stop, "old-generation").unwrap();
        assert_eq!(
            read_handshake_token(&ready).unwrap().as_deref(),
            Some("new-generation")
        );
        assert!(!stop.exists());
    }
}
