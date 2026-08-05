//! Verified, transactional per-user installation for Open Hub release bundles.

pub mod bundle;
pub mod manifest;
pub mod platform;
pub mod transaction;

pub use bundle::{Bundle, ValidatedBundle};
pub use manifest::{
    Architecture, BundleFile, BundleManifest, Component, ComponentManifest, InstallRoot,
    InstallState, InstalledFile, Platform,
};
pub use platform::{NativePlatform, PlatformBackend, PlatformPaths, RegistrationState};
pub use transaction::{
    FaultInjector, InstallReport, NoFaults, PreparedFile, TransactionPhase, TransactionPlan, apply,
    apply_with_injector, load_state, uninstall, validate_installed_state_paths,
};
