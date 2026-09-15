use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{
    manifest::{Component, InstallState},
    platform::{PlatformBackend, PlatformPaths, RegistrationRequest, RegistrationState},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionPhase {
    Recover,
    Stage,
    Shutdown,
    Swap,
    Registration,
    Path,
    Manifest,
}

pub trait FaultInjector {
    fn checkpoint(&self, _phase: TransactionPhase) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoFaults;

impl FaultInjector for NoFaults {}

#[derive(Clone, Debug)]
pub struct PreparedFile {
    pub component: Option<Component>,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub executable: bool,
}

#[derive(Clone, Debug)]
pub struct TransactionPlan {
    pub paths: PlatformPaths,
    pub selected_components: BTreeSet<Component>,
    pub files: Vec<PreparedFile>,
    pub remove_paths: Vec<PathBuf>,
    pub previous_registrations: RegistrationState,
    pub new_state: InstallState,
    pub stop_tray: bool,
}

#[derive(Clone, Debug)]
pub struct InstallReport {
    pub state: InstallState,
    pub repaired_files: usize,
    pub removed_files: usize,
}

pub fn apply(platform: &impl PlatformBackend, plan: TransactionPlan) -> Result<InstallReport> {
    apply_with_injector(platform, plan, &NoFaults)
}

pub fn apply_with_injector(
    platform: &impl PlatformBackend,
    mut plan: TransactionPlan,
    injector: &impl FaultInjector,
) -> Result<InstallReport> {
    recover_stale(platform, &plan.paths)?;
    injector.checkpoint(TransactionPhase::Recover)?;
    plan.new_state.validate()?;
    if plan.new_state.selected_components != plan.selected_components {
        bail!("transaction component selection differs from the installed state");
    }
    validate_plan_paths(&plan)?;

    let mut staged = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        let stage = artifact_path(&file.destination, "stage")?;
        if let Some(parent) = stage.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
        fs::copy(&file.source, &stage).with_context(|| {
            format!(
                "failed to stage `{}` as `{}`",
                file.source.display(),
                stage.display()
            )
        })?;
        let staged_file = fs::OpenOptions::new().write(true).open(&stage)?;
        staged_file.sync_all()?;
        set_executable(&stage, file.executable)?;
        staged.push((stage, file.destination.clone()));
    }
    injector
        .checkpoint(TransactionPhase::Stage)
        .inspect_err(|_| cleanup_stages(&staged))?;

    if plan.stop_tray {
        platform.request_tray_shutdown()?;
    }
    platform.request_agent_shutdown()?;
    injector
        .checkpoint(TransactionPhase::Shutdown)
        .inspect_err(|_| cleanup_stages(&staged))?;

    let actual_registrations = platform.snapshot_registrations()?;
    let registration_ownership =
        safe_ownership_view(&actual_registrations, &plan.previous_registrations);
    let journal_path = transaction_journal_path(&plan.paths);
    let mut journal_entries = plan
        .files
        .iter()
        .map(|file| JournalEntry::new(file.destination.clone(), true))
        .chain(
            plan.remove_paths
                .iter()
                .cloned()
                .map(|path| JournalEntry::new(path, false)),
        )
        .collect::<Result<Vec<_>>>()?;
    journal_entries.push(JournalEntry::new(plan.paths.state_file.clone(), true)?);
    write_json_atomic(
        &journal_path,
        &RecoveryJournal {
            registrations: actual_registrations.clone(),
            entries: journal_entries,
        },
    )?;
    let mut swaps = Vec::new();
    let mutate = (|| -> Result<()> {
        for (stage, target) in &staged {
            swaps.push(swap_in(stage, target)?);
        }
        for target in &plan.remove_paths {
            if target.exists() {
                swaps.push(remove_with_backup(target)?);
            }
        }
        injector.checkpoint(TransactionPhase::Swap)?;

        let registrations = platform.reconcile_registrations(
            &RegistrationRequest {
                components: plan.selected_components.clone(),
                paths: plan.paths.clone(),
            },
            &registration_ownership,
        )?;
        injector.checkpoint(TransactionPhase::Registration)?;
        injector.checkpoint(TransactionPhase::Path)?;
        plan.new_state.registrations = registrations;
        let state_stage = artifact_path(&plan.paths.state_file, "stage")?;
        write_json_file(&state_stage, &plan.new_state)?;
        swaps.push(swap_in(&state_stage, &plan.paths.state_file)?);
        injector.checkpoint(TransactionPhase::Manifest)?;
        fs::remove_file(&journal_path).with_context(|| {
            format!(
                "failed to commit transaction journal `{}`",
                journal_path.display()
            )
        })?;
        Ok(())
    })();

    if let Err(error) = mutate {
        let registration_error = platform.restore_registrations(&actual_registrations).err();
        let rollback_error = rollback_swaps(&swaps).err();
        cleanup_stages(&staged);
        let _ = fs::remove_file(&journal_path);
        if let Some(rollback) = rollback_error {
            return Err(error).context(format!("file rollback also failed: {rollback:#}"));
        }
        if let Some(registration) = registration_error {
            return Err(error).context(format!(
                "registration rollback also failed: {registration:#}"
            ));
        }
        return Err(error);
    }

    finish_swaps(&swaps)?;
    cleanup_stages(&staged);
    for target in &plan.remove_paths {
        prune_managed_application_parents(target, &plan.paths)?;
    }
    Ok(InstallReport {
        state: plan.new_state,
        repaired_files: plan.files.len(),
        removed_files: plan.remove_paths.len(),
    })
}

fn safe_ownership_view(
    observed: &RegistrationState,
    installed: &RegistrationState,
) -> RegistrationState {
    let mut view = observed.clone();
    for observed_record in &mut view.records {
        observed_record.owned = installed.records.iter().any(|installed_record| {
            installed_record.owned
                && installed_record.kind == observed_record.kind
                && installed_record.location == observed_record.location
                && installed_record.value == observed_record.value
        });
    }
    view.path_warning = observed.path_warning.clone();
    view
}

fn validate_state_targets(paths: &PlatformPaths, state: &InstallState) -> Result<()> {
    for file in &state.files {
        let target = paths.resolve(file.root).join(&file.destination);
        validate_target_containment(&target, &paths.allowed_roots(), false)?;
        let expected_root = paths.resolve(file.root);
        if !target.starts_with(expected_root) {
            bail!(
                "installed-state target `{}` is outside its declared root",
                target.display()
            );
        }
    }
    validate_target_containment(
        &paths.state_file,
        std::slice::from_ref(&paths.install_root),
        false,
    )?;
    Ok(())
}

pub fn validate_installed_state_paths(paths: &PlatformPaths, state: &InstallState) -> Result<()> {
    validate_state_targets(paths, state)
}

pub fn uninstall(
    platform: &impl PlatformBackend,
    paths: &PlatformPaths,
    state: &InstallState,
    remove_user_data: bool,
) -> Result<Vec<PathBuf>> {
    validate_state_targets(paths, state)?;
    let actual = platform.snapshot_registrations()?;
    let registration_ownership = safe_ownership_view(&actual, &state.registrations);
    let empty = BTreeSet::new();
    platform.reconcile_registrations(
        &RegistrationRequest {
            components: empty,
            paths: paths.clone(),
        },
        &registration_ownership,
    )?;
    if state.selected_components.contains(&Component::Tray)
        && let Err(error) = platform.request_tray_shutdown()
    {
        platform.restore_registrations(&actual)?;
        return Err(error);
    }
    if let Err(error) = platform.request_agent_shutdown() {
        platform.restore_registrations(&actual)?;
        return Err(error);
    }

    let mut preserved = Vec::new();
    let mut removed_targets = Vec::new();
    for file in &state.files {
        let target = paths.resolve(file.root).join(&file.destination);
        if target.is_file() || target.symlink_metadata().is_ok() {
            fs::remove_file(&target)
                .with_context(|| format!("failed to remove managed file `{}`", target.display()))?;
        }
        removed_targets.push(target);
    }
    for target in &removed_targets {
        prune_managed_application_parents(target, paths)?;
    }
    if paths.state_file.is_file() {
        fs::remove_file(&paths.state_file).with_context(|| {
            format!(
                "failed to remove installed state `{}`",
                paths.state_file.display()
            )
        })?;
    }
    if remove_user_data {
        remove_data_path(&paths.preferences)?;
        remove_data_path(&paths.logs)?;
    } else {
        if paths.preferences.exists() {
            preserved.push(paths.preferences.clone());
        }
        if paths.logs.exists() {
            preserved.push(paths.logs.clone());
        }
    }
    Ok(preserved)
}

pub fn load_state(path: &Path) -> Result<Option<InstallState>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read `{}`", path.display()));
        }
    };
    let state: InstallState = serde_json::from_slice(&bytes)
        .with_context(|| format!("installed state `{}` is invalid", path.display()))?;
    state.validate()?;
    Ok(Some(state))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .context("installed-state path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to persist `{}`", path.display()))?;
    Ok(())
}

fn write_json_file(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .context("transaction file path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut file = fs::File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[derive(Clone, Debug)]
struct Swap {
    target: PathBuf,
    backup: Option<PathBuf>,
    installed_new: bool,
}

fn swap_in(stage: &Path, target: &Path) -> Result<Swap> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup = if target.exists() {
        let backup = artifact_path(target, "backup")?;
        if backup.exists() {
            remove_data_path(&backup)?;
        }
        retry_rename(target, &backup)?;
        Some(backup)
    } else {
        None
    };
    if let Err(error) = retry_rename(stage, target) {
        if let Some(backup) = &backup {
            let _ = retry_rename(backup, target);
        }
        return Err(error);
    }
    Ok(Swap {
        target: target.to_path_buf(),
        backup,
        installed_new: true,
    })
}

fn remove_with_backup(target: &Path) -> Result<Swap> {
    let backup = artifact_path(target, "backup")?;
    if backup.exists() {
        remove_data_path(&backup)?;
    }
    retry_rename(target, &backup)?;
    Ok(Swap {
        target: target.to_path_buf(),
        backup: Some(backup),
        installed_new: false,
    })
}

fn rollback_swaps(swaps: &[Swap]) -> Result<()> {
    for swap in swaps.iter().rev() {
        if swap.installed_new && swap.target.exists() {
            remove_data_path(&swap.target)?;
        }
        if let Some(backup) = &swap.backup
            && backup.exists()
        {
            retry_rename(backup, &swap.target)?;
        }
    }
    Ok(())
}

fn finish_swaps(swaps: &[Swap]) -> Result<()> {
    for swap in swaps {
        if let Some(backup) = &swap.backup
            && backup.exists()
        {
            remove_data_path(backup)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecoveryJournal {
    registrations: RegistrationState,
    entries: Vec<JournalEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct JournalEntry {
    target: PathBuf,
    backup: PathBuf,
    existed: bool,
    installed_new: bool,
}

impl JournalEntry {
    fn new(target: PathBuf, installed_new: bool) -> Result<Self> {
        Ok(Self {
            backup: artifact_path(&target, "backup")?,
            existed: target.exists(),
            target,
            installed_new,
        })
    }
}

fn transaction_journal_path(paths: &PlatformPaths) -> PathBuf {
    paths.install_root.join("install-transaction.json")
}

fn recover_stale(platform: &impl PlatformBackend, paths: &PlatformPaths) -> Result<()> {
    let journal_path = transaction_journal_path(paths);
    let bytes = match fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to read transaction recovery journal"),
    };
    let journal: RecoveryJournal = serde_json::from_slice(&bytes)
        .context("transaction recovery journal is invalid; refusing an unsafe repair")?;
    validate_recovery_journal(paths, &journal)?;
    platform.restore_registrations(&journal.registrations)?;
    for entry in journal.entries.iter().rev() {
        let stage = artifact_path(&entry.target, "stage")?;
        if stage.exists() {
            remove_data_path(&stage)?;
        }
        if entry.backup.exists() {
            if entry.target.exists() {
                remove_data_path(&entry.target)?;
            }
            retry_rename(&entry.backup, &entry.target)?;
        } else if entry.installed_new && !entry.existed && entry.target.exists() {
            remove_data_path(&entry.target)?;
        }
    }
    fs::remove_file(&journal_path).context("failed to remove recovered transaction journal")?;
    Ok(())
}

fn validate_recovery_journal(paths: &PlatformPaths, journal: &RecoveryJournal) -> Result<()> {
    let mut targets = BTreeSet::new();
    let allowed_roots = paths.allowed_roots();
    for entry in &journal.entries {
        if !targets.insert(entry.target.clone()) {
            bail!(
                "transaction recovery journal repeats target `{}`",
                entry.target.display()
            );
        }
        let roots = if entry.target == paths.state_file {
            std::slice::from_ref(&paths.install_root)
        } else {
            &allowed_roots
        };
        validate_target_containment(&entry.target, roots, false)?;
        let expected_backup = artifact_path(&entry.target, "backup")?;
        if entry.backup != expected_backup {
            bail!(
                "transaction recovery backup `{}` does not match target `{}`",
                entry.backup.display(),
                entry.target.display()
            );
        }
        validate_target_containment(&entry.backup, roots, false)?;
        validate_target_containment(&artifact_path(&entry.target, "stage")?, roots, false)?;
    }
    Ok(())
}

fn validate_plan_paths(plan: &TransactionPlan) -> Result<()> {
    let allowed_roots = plan.paths.allowed_roots();
    for target in plan
        .files
        .iter()
        .map(|file| &file.destination)
        .chain(plan.remove_paths.iter())
    {
        validate_target_containment(target, &allowed_roots, false)?;
        validate_target_containment(&artifact_path(target, "stage")?, &allowed_roots, false)?;
        validate_target_containment(&artifact_path(target, "backup")?, &allowed_roots, false)?;
    }
    for file in &plan.files {
        let root = matching_root(&file.destination, &allowed_roots)?;
        fs::create_dir_all(root).with_context(|| {
            format!(
                "failed to create selected install root `{}`",
                root.display()
            )
        })?;
    }
    fs::create_dir_all(&plan.paths.install_root).with_context(|| {
        format!(
            "failed to create trusted installation root `{}`",
            plan.paths.install_root.display()
        )
    })?;
    validate_target_containment(
        &plan.paths.state_file,
        std::slice::from_ref(&plan.paths.install_root),
        true,
    )?;
    validate_target_containment(
        &transaction_journal_path(&plan.paths),
        std::slice::from_ref(&plan.paths.install_root),
        true,
    )
}

fn matching_root<'a>(target: &Path, allowed_roots: &'a [PathBuf]) -> Result<&'a PathBuf> {
    allowed_roots
        .iter()
        .filter(|root| target.starts_with(root))
        .max_by_key(|root| root.components().count())
        .with_context(|| {
            format!(
                "managed destination `{}` is outside every trusted root",
                target.display()
            )
        })
}

fn validate_target_containment(
    target: &Path,
    allowed_roots: &[PathBuf],
    create_root: bool,
) -> Result<()> {
    let root = matching_root(target, allowed_roots)?;
    if create_root {
        fs::create_dir_all(root).with_context(|| {
            format!("failed to create trusted install root `{}`", root.display())
        })?;
    }
    let canonical_root = canonical_virtual(root)?;
    let relative = target.strip_prefix(root)?;
    let mut current = canonical_root.clone();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(component) = component else {
            bail!(
                "managed destination `{}` contains path traversal",
                target.display()
            );
        };
        current.push(component);
        if index + 1 == components.len() {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(_) => {
                let resolved = current.canonicalize().with_context(|| {
                    format!(
                        "failed to resolve managed destination `{}`",
                        current.display()
                    )
                })?;
                if !resolved.starts_with(&canonical_root) {
                    bail!(
                        "managed destination `{}` escapes trusted root `{}` through a symlink",
                        target.display(),
                        root.display()
                    );
                }
                current = resolved;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to inspect managed destination"),
        }
    }
    Ok(())
}

fn canonical_virtual(path: &Path) -> Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .context("trusted path has no existing ancestor")?;
        missing.push(name.to_os_string());
        existing = existing
            .parent()
            .context("trusted path has no existing parent")?;
    }
    let mut resolved = existing.canonicalize().with_context(|| {
        format!(
            "failed to resolve trusted ancestor `{}`",
            existing.display()
        )
    })?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn cleanup_stages(stages: &[(PathBuf, PathBuf)]) {
    for (stage, _) in stages {
        let _ = remove_data_path(stage);
    }
}

fn artifact_path(target: &Path, suffix: &str) -> Result<PathBuf> {
    let filename = target
        .file_name()
        .context("managed file path has no filename")?;
    Ok(target.with_file_name(format!(".{}.gflick-{suffix}", filename.to_string_lossy())))
}

fn retry_rename(from: &Path, to: &Path) -> Result<()> {
    let mut last = None;
    for _ in 0..20 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if is_locked(&error) => {
                last = Some(error);
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to move `{}` to `{}`", from.display(), to.display())
                });
            }
        }
    }
    Err(last.context("managed file remained locked")?.into())
}

fn is_locked(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

fn set_executable(path: &Path, executable: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        let mode = permissions.mode();
        permissions.set_mode(if executable {
            mode | 0o755
        } else {
            mode & !0o111
        });
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    let _ = (path, executable);
    Ok(())
}

fn remove_data_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn prune_managed_application_parents(target: &Path, paths: &PlatformPaths) -> Result<()> {
    let root = if target.starts_with(&paths.user_applications) {
        &paths.user_applications
    } else if target.starts_with(&paths.private_app) {
        &paths.private_app
    } else {
        return Ok(());
    };
    let Some(mut directory) = target.parent() else {
        return Ok(());
    };
    while directory.starts_with(root) {
        match fs::remove_dir(directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                if error.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                    break;
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to prune managed application directory `{}`",
                        directory.display()
                    )
                });
            }
        }
        if directory == root {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        manifest::{InstallRoot, InstalledFile, STATE_SCHEMA},
        platform::{RegistrationKind, RegistrationRecord},
    };
    use std::cell::{Cell, RefCell};

    struct FakePlatform {
        paths: PlatformPaths,
        registrations: RefCell<RegistrationState>,
        offline: bool,
        tray_shutdowns: Cell<usize>,
    }

    impl PlatformBackend for FakePlatform {
        fn paths(&self) -> Result<PlatformPaths> {
            Ok(self.paths.clone())
        }
        fn snapshot_registrations(&self) -> Result<RegistrationState> {
            Ok(self.registrations.borrow().clone())
        }
        fn reconcile_registrations(
            &self,
            request: &RegistrationRequest,
            _previous: &RegistrationState,
        ) -> Result<RegistrationState> {
            let mut state = RegistrationState::default();
            for (component, kind) in [
                (Component::Agent, RegistrationKind::AgentStartup),
                (Component::Tray, RegistrationKind::TrayStartup),
                (Component::Cli, RegistrationKind::CliExposure),
            ] {
                if request.components.contains(&component) {
                    state.records.push(RegistrationRecord {
                        kind,
                        location: kind_location(kind),
                        value: component.to_string(),
                        owned: true,
                    });
                }
            }
            *self.registrations.borrow_mut() = state.clone();
            Ok(state)
        }
        fn restore_registrations(&self, snapshot: &RegistrationState) -> Result<()> {
            *self.registrations.borrow_mut() = snapshot.clone();
            Ok(())
        }
        fn request_agent_shutdown(&self) -> Result<()> {
            let _ = self.offline;
            Ok(())
        }
        fn request_tray_shutdown(&self) -> Result<()> {
            self.tray_shutdowns.set(self.tray_shutdowns.get() + 1);
            Ok(())
        }
    }

    fn kind_location(kind: RegistrationKind) -> PathBuf {
        PathBuf::from(format!("{kind:?}"))
    }

    struct FailAt(TransactionPhase);
    impl FaultInjector for FailAt {
        fn checkpoint(&self, phase: TransactionPhase) -> Result<()> {
            if phase == self.0 {
                bail!("injected {phase:?} failure")
            } else {
                Ok(())
            }
        }
    }

    fn fixture() -> (tempfile::TempDir, FakePlatform, TransactionPlan, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("install");
        let paths = PlatformPaths {
            install_root: root.clone(),
            private_bin: root.join("bin"),
            private_app: root.join("app"),
            user_applications: temp.path().join("Applications"),
            user_local_bin: temp.path().join(".local/bin"),
            state_file: root.join("install-state.json"),
            tray_ready: root.join("tray.ready"),
            tray_stop: root.join("tray.stop"),
            preferences: temp.path().join("config/settings.json"),
            logs: temp.path().join("logs"),
        };
        fs::create_dir_all(&paths.private_bin).unwrap();
        let source = temp.path().join("new-agent");
        fs::write(&source, b"new").unwrap();
        let destination = paths.private_bin.join("gflick-agent");
        fs::write(&destination, b"old").unwrap();
        let old_regs = RegistrationState {
            records: vec![RegistrationRecord {
                kind: RegistrationKind::AgentStartup,
                location: "agent".into(),
                value: "old".into(),
                owned: true,
            }],
            path_warning: None,
        };
        let platform = FakePlatform {
            paths: paths.clone(),
            registrations: RefCell::new(old_regs.clone()),
            offline: true,
            tray_shutdowns: Cell::new(0),
        };
        let old_state = InstallState {
            schema: STATE_SCHEMA,
            product_version: "1".into(),
            selected_components: BTreeSet::from([Component::Agent]),
            files: vec![
                InstalledFile {
                    component: None,
                    root: InstallRoot::PrivateBin,
                    destination: "gflick-setup".into(),
                    length: 3,
                    sha256: "10".repeat(32),
                    executable: true,
                },
                InstalledFile {
                    component: Some(Component::Agent),
                    root: InstallRoot::PrivateBin,
                    destination: "gflick-agent".into(),
                    length: 3,
                    sha256: "11".repeat(32),
                    executable: true,
                },
            ],
            registrations: old_regs.clone(),
        };
        write_json_atomic(&paths.state_file, &old_state).unwrap();
        let state = InstallState {
            schema: STATE_SCHEMA,
            product_version: "2".into(),
            selected_components: BTreeSet::from([Component::Agent]),
            files: vec![
                InstalledFile {
                    component: None,
                    root: InstallRoot::PrivateBin,
                    destination: "gflick-setup".into(),
                    length: 3,
                    sha256: "10".repeat(32),
                    executable: true,
                },
                InstalledFile {
                    component: Some(Component::Agent),
                    root: InstallRoot::PrivateBin,
                    destination: "gflick-agent".into(),
                    length: 3,
                    sha256: "00".repeat(32),
                    executable: true,
                },
            ],
            registrations: RegistrationState::default(),
        };
        let plan = TransactionPlan {
            paths,
            selected_components: BTreeSet::from([Component::Agent]),
            files: vec![PreparedFile {
                component: Some(Component::Agent),
                source,
                destination: destination.clone(),
                executable: true,
            }],
            remove_paths: vec![],
            previous_registrations: old_regs,
            new_state: state,
            stop_tray: true,
        };
        (temp, platform, plan, destination)
    }

    #[test]
    fn success_is_complete_new_state_and_offline_agent_is_not_error() {
        let (_temp, platform, plan, destination) = fixture();
        let report = apply(&platform, plan).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"new");
        assert_eq!(platform.tray_shutdowns.get(), 1);
        assert_eq!(
            load_state(&platform.paths.state_file).unwrap().unwrap(),
            report.state
        );
        assert!(!platform.paths.private_app.exists());
        assert!(!platform.paths.user_applications.exists());
        assert!(!platform.paths.user_local_bin.exists());
    }

    #[test]
    fn failures_leave_complete_old_state() {
        for phase in [
            TransactionPhase::Stage,
            TransactionPhase::Shutdown,
            TransactionPhase::Swap,
            TransactionPhase::Registration,
            TransactionPhase::Path,
            TransactionPhase::Manifest,
        ] {
            let (_temp, platform, plan, destination) = fixture();
            assert!(apply_with_injector(&platform, plan, &FailAt(phase)).is_err());
            assert_eq!(fs::read(destination).unwrap(), b"old", "phase {phase:?}");
            assert_eq!(
                load_state(&platform.paths.state_file)
                    .unwrap()
                    .unwrap()
                    .product_version,
                "1"
            );
            assert_eq!(platform.registrations.borrow().records[0].value, "old");
        }
    }

    #[test]
    fn nested_tree_and_owned_removal_are_transactional() {
        let (_temp, platform, mut plan, _) = fixture();
        let nested_source = platform.paths.install_root.join("icon-source");
        fs::write(&nested_source, b"icon").unwrap();
        let nested = platform
            .paths
            .user_applications
            .join("gflick.app/Contents/Resources/icon.icns");
        plan.files.push(PreparedFile {
            component: Some(Component::Settings),
            source: nested_source,
            destination: nested.clone(),
            executable: false,
        });
        plan.selected_components.insert(Component::Settings);
        plan.new_state
            .selected_components
            .insert(Component::Settings);
        plan.new_state.files.push(InstalledFile {
            component: Some(Component::Settings),
            root: InstallRoot::UserApplications,
            destination: "gflick.app/Contents/Resources/icon.icns".into(),
            length: 4,
            sha256: "33".repeat(32),
            executable: false,
        });
        apply(&platform, plan).unwrap();
        assert_eq!(fs::read(nested).unwrap(), b"icon");
        assert!(platform.paths.user_applications.exists());
        assert!(!platform.paths.private_app.exists());
        assert!(!platform.paths.user_local_bin.exists());
    }

    #[test]
    fn installs_the_complete_macos_tray_application_tree() {
        let (temp, platform, mut plan, _) = fixture();
        for (source_name, destination, bytes, executable) in [
            (
                "tray-source",
                "Contents/MacOS/gflick-tray",
                b"tray".as_slice(),
                true,
            ),
            (
                "plist-source",
                "Contents/Info.plist",
                b"plist".as_slice(),
                false,
            ),
            (
                "icon-source",
                "Contents/Resources/favicon.icns",
                b"icon".as_slice(),
                false,
            ),
        ] {
            let source = temp.path().join(source_name);
            fs::write(&source, bytes).unwrap();
            plan.files.push(PreparedFile {
                component: Some(Component::Tray),
                source,
                destination: platform.paths.private_app.join(destination),
                executable,
            });
            plan.new_state.files.push(InstalledFile {
                component: Some(Component::Tray),
                root: InstallRoot::PrivateApp,
                destination: destination.into(),
                length: bytes.len() as u64,
                sha256: "22".repeat(32),
                executable,
            });
        }
        plan.selected_components.insert(Component::Tray);
        plan.new_state.selected_components.insert(Component::Tray);

        apply(&platform, plan).unwrap();
        assert_eq!(
            fs::read(
                platform
                    .paths
                    .private_app
                    .join("Contents/MacOS/gflick-tray")
            )
            .unwrap(),
            b"tray"
        );
        assert_eq!(
            fs::read(platform.paths.private_app.join("Contents/Info.plist")).unwrap(),
            b"plist"
        );
        assert_eq!(
            fs::read(
                platform
                    .paths
                    .private_app
                    .join("Contents/Resources/favicon.icns")
            )
            .unwrap(),
            b"icon"
        );
    }

    #[test]
    fn normal_uninstall_preserves_data_and_explicit_removal_deletes_it() {
        for remove in [false, true] {
            let (_temp, platform, plan, _) = fixture();
            let state = apply(&platform, plan).unwrap().state;
            fs::create_dir_all(platform.paths.preferences.parent().unwrap()).unwrap();
            fs::write(&platform.paths.preferences, b"settings").unwrap();
            fs::create_dir_all(&platform.paths.logs).unwrap();
            fs::write(platform.paths.logs.join("agent.log"), b"log").unwrap();
            let preserved = uninstall(&platform, &platform.paths, &state, remove).unwrap();
            assert_eq!(platform.paths.preferences.exists(), !remove);
            assert_eq!(platform.paths.logs.exists(), !remove);
            assert_eq!(preserved.is_empty(), remove);
        }
    }

    #[test]
    fn registry_lock_codes_are_bounded_retry_candidates() {
        assert!(is_locked(&std::io::Error::from_raw_os_error(32)));
        assert!(is_locked(&std::io::Error::from_raw_os_error(33)));
        assert!(!is_locked(&std::io::Error::from_raw_os_error(2)));
    }

    #[test]
    fn ownership_requires_an_exact_observed_match_and_never_synthesizes() {
        let installed = RegistrationState {
            records: vec![RegistrationRecord {
                kind: RegistrationKind::SettingsLauncher,
                location: "gflick.lnk".into(),
                value: "gflick-settings.exe".into(),
                owned: true,
            }],
            path_warning: None,
        };
        let exact = installed.clone();
        assert!(
            safe_ownership_view(&exact, &installed)
                .record(RegistrationKind::SettingsLauncher)
                .unwrap()
                .owned
        );
        let changed = RegistrationState {
            records: vec![RegistrationRecord {
                kind: RegistrationKind::SettingsLauncher,
                location: "gflick.lnk".into(),
                value: "user-settings.exe".into(),
                owned: false,
            }],
            path_warning: None,
        };
        let changed_view = safe_ownership_view(&changed, &installed);
        let changed_record = changed_view
            .record(RegistrationKind::SettingsLauncher)
            .unwrap();
        assert!(!changed_record.owned);
        assert_eq!(changed_record.value, "user-settings.exe");
        assert!(
            safe_ownership_view(&RegistrationState::default(), &installed)
                .records
                .is_empty()
        );
    }

    #[test]
    fn injected_rollback_restores_exact_changed_observed_registration() {
        let (_temp, platform, plan, _) = fixture();
        *platform.registrations.borrow_mut() = RegistrationState {
            records: vec![RegistrationRecord {
                kind: RegistrationKind::AgentStartup,
                location: kind_location(RegistrationKind::AgentStartup),
                value: "user-changed-command".into(),
                owned: false,
            }],
            path_warning: Some("user warning".into()),
        };
        assert!(
            apply_with_injector(&platform, plan, &FailAt(TransactionPhase::Registration)).is_err()
        );
        let restored = platform.registrations.borrow();
        assert_eq!(restored.records[0].value, "user-changed-command");
        assert_eq!(restored.path_warning.as_deref(), Some("user warning"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_destination_that_escapes_through_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let (_temp, platform, mut plan, _) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let linked = platform.paths.private_bin.join("linked");
        symlink(outside.path(), &linked).unwrap();
        plan.files[0].destination = linked.join("agent");
        assert!(apply(&platform, plan).is_err());
        assert!(!outside.path().join("agent").exists());
    }

    #[test]
    fn stale_journal_rolls_back_files_and_installed_state() {
        let (_temp, platform, plan, destination) = fixture();
        let old_state = fs::read(&platform.paths.state_file).unwrap();
        let backup = artifact_path(&destination, "backup").unwrap();
        fs::rename(&destination, &backup).unwrap();
        fs::write(&destination, b"new").unwrap();
        let state_backup = artifact_path(&platform.paths.state_file, "backup").unwrap();
        fs::rename(&platform.paths.state_file, &state_backup).unwrap();
        fs::write(&platform.paths.state_file, b"new-state").unwrap();
        write_json_atomic(
            &transaction_journal_path(&platform.paths),
            &RecoveryJournal {
                registrations: plan.previous_registrations,
                entries: vec![
                    JournalEntry {
                        target: destination.clone(),
                        backup,
                        existed: true,
                        installed_new: true,
                    },
                    JournalEntry {
                        target: platform.paths.state_file.clone(),
                        backup: state_backup,
                        existed: true,
                        installed_new: true,
                    },
                ],
            },
        )
        .unwrap();
        recover_stale(&platform, &platform.paths).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"old");
        assert_eq!(fs::read(&platform.paths.state_file).unwrap(), old_state);
    }

    #[test]
    fn malicious_recovery_paths_are_rejected_before_registrations_or_files_change() {
        let (temp, platform, plan, _) = fixture();
        let outside = temp.path().join("outside-sentinel");
        fs::write(&outside, b"keep").unwrap();
        let registrations_before = platform.registrations.borrow().clone();
        write_json_atomic(
            &transaction_journal_path(&platform.paths),
            &RecoveryJournal {
                registrations: RegistrationState::default(),
                entries: vec![JournalEntry {
                    target: outside.clone(),
                    backup: artifact_path(&outside, "backup").unwrap(),
                    existed: true,
                    installed_new: true,
                }],
            },
        )
        .unwrap();
        assert!(recover_stale(&platform, &platform.paths).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"keep");
        assert_eq!(*platform.registrations.borrow(), registrations_before);
        assert!(plan.paths.state_file.exists());
    }

    #[test]
    fn malicious_installed_state_path_is_rejected_before_uninstall_mutation() {
        let (temp, platform, plan, _) = fixture();
        let outside = temp.path().join("outside-state-sentinel");
        fs::write(&outside, b"keep").unwrap();
        let mut state = plan.new_state;
        state.files[0].destination = outside.clone();
        let registrations_before = platform.registrations.borrow().clone();
        assert!(uninstall(&platform, &platform.paths, &state, false).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"keep");
        assert_eq!(*platform.registrations.borrow(), registrations_before);
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_rejects_symlinked_parent_escape_before_mutation() {
        use std::os::unix::fs::symlink;

        let (_temp, platform, plan, _) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, b"keep").unwrap();
        fs::create_dir_all(&platform.paths.private_app).unwrap();
        symlink(outside.path(), platform.paths.private_app.join("linked")).unwrap();
        let mut state = plan.new_state;
        state.files[0].root = InstallRoot::PrivateApp;
        state.files[0].destination = "linked/sentinel".into();
        let registrations_before = platform.registrations.borrow().clone();
        assert!(uninstall(&platform, &platform.paths, &state, false).is_err());
        assert_eq!(fs::read(sentinel).unwrap(), b"keep");
        assert_eq!(*platform.registrations.borrow(), registrations_before);
    }

    #[test]
    fn prunes_empty_managed_app_tree_but_preserves_unrelated_sentinel() {
        let (_temp, platform, _plan, _) = fixture();
        let managed = platform
            .paths
            .private_app
            .join("Contents/Resources/icon.icns");
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::write(&managed, b"icon").unwrap();
        fs::remove_file(&managed).unwrap();
        prune_managed_application_parents(&managed, &platform.paths).unwrap();
        assert!(!platform.paths.private_app.exists());

        let managed = platform
            .paths
            .user_applications
            .join("Contents/MacOS/gflick");
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::write(&managed, b"settings").unwrap();
        let sentinel = platform.paths.user_applications.join("unrelated.txt");
        fs::write(&sentinel, b"keep").unwrap();
        fs::remove_file(&managed).unwrap();
        prune_managed_application_parents(&managed, &platform.paths).unwrap();
        assert_eq!(fs::read(sentinel).unwrap(), b"keep");
    }
}
