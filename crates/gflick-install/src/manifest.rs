use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Component as PathComponent, Path, PathBuf},
    str::FromStr,
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::platform::RegistrationState;

pub const BUNDLE_SCHEMA: u32 = 1;
pub const STATE_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Agent,
    Settings,
    Tray,
    Cli,
}

impl Component {
    pub const ALL: [Self; 4] = [Self::Agent, Self::Settings, Self::Tray, Self::Cli];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Settings => "settings",
            Self::Tray => "tray",
            Self::Cli => "cli",
        }
    }
}

impl fmt::Display for Component {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Component {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "agent" => Ok(Self::Agent),
            "settings" => Ok(Self::Settings),
            "tray" => Ok(Self::Tray),
            "cli" => Ok(Self::Cli),
            other => {
                bail!("unknown install component `{other}`; expected agent, settings, tray, or cli")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallRoot {
    PrivateBin,
    PrivateApp,
    UserApplications,
    UserLocalBin,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    Macos,
}

impl Platform {
    #[must_use]
    pub const fn current() -> Option<Self> {
        if cfg!(windows) {
            Some(Self::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Self::Macos)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
}

impl Architecture {
    #[must_use]
    pub const fn current() -> Option<Self> {
        if cfg!(target_arch = "x86_64") {
            Some(Self::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Self::Aarch64)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    /// Relative path below the extracted bundle root.
    pub source: PathBuf,
    pub root: InstallRoot,
    /// Relative destination below `root`.
    pub destination: PathBuf,
    pub length: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentManifest {
    pub files: Vec<BundleFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub schema: u32,
    pub product_version: String,
    pub platform: Platform,
    pub arch: Architecture,
    pub required: BTreeSet<Component>,
    pub defaults: BTreeSet<Component>,
    pub setup: BundleFile,
    pub components: BTreeMap<Component, ComponentManifest>,
}

impl BundleManifest {
    pub fn validate_model(
        &self,
        expected_platform: Platform,
        expected_arch: Architecture,
    ) -> Result<()> {
        if self.schema != BUNDLE_SCHEMA {
            bail!(
                "unsupported bundle schema {}; expected {BUNDLE_SCHEMA}",
                self.schema
            );
        }
        if self.product_version.trim().is_empty() {
            bail!("bundle product_version must not be empty");
        }
        if self.platform != expected_platform {
            bail!(
                "bundle platform {:?} is incompatible with this {:?} runtime",
                self.platform,
                expected_platform
            );
        }
        if self.arch != expected_arch {
            bail!(
                "bundle architecture {:?} is incompatible with this {:?} runtime",
                self.arch,
                expected_arch
            );
        }
        if expected_platform == Platform::Macos && expected_arch != Architecture::Aarch64 {
            bail!("Intel macOS is unsupported; use an Apple Silicon bundle and runtime");
        }
        if self.required != BTreeSet::from([Component::Agent]) {
            bail!("bundle required set must contain exactly `agent`");
        }
        if !self.components.contains_key(&Component::Agent) {
            bail!("bundle is missing the required agent payload");
        }
        for component in &self.required {
            if !self.is_available(*component) {
                bail!("required component `{component}` has no payload files");
            }
        }
        for component in &self.defaults {
            if !self.is_available(*component) {
                bail!("default component `{component}` is unavailable in this bundle");
            }
        }
        if !self.defaults.is_superset(&self.required) {
            bail!("bundle defaults must include every required component");
        }
        self.validate_file_metadata()?;
        if expected_platform == Platform::Macos && self.is_available(Component::Tray) {
            self.validate_macos_tray_bundle()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn is_available(&self, component: Component) -> bool {
        self.components
            .get(&component)
            .is_some_and(|manifest| !manifest.files.is_empty())
    }

    pub fn validate_selection(&self, selected: &BTreeSet<Component>) -> Result<()> {
        if !selected.contains(&Component::Agent) {
            bail!("the final component set must include required component `agent`");
        }
        for component in selected {
            if !self.is_available(*component) {
                bail!("component `{component}` is unavailable in this bundle");
            }
        }
        Ok(())
    }

    pub fn files_for(
        &self,
        selected: &BTreeSet<Component>,
    ) -> Vec<(Option<Component>, &BundleFile)> {
        let mut files = vec![(None, &self.setup)];
        for component in Component::ALL {
            if selected.contains(&component)
                && let Some(manifest) = self.components.get(&component)
            {
                files.extend(manifest.files.iter().map(|file| (Some(component), file)));
            }
        }
        files
    }

    fn validate_file_metadata(&self) -> Result<()> {
        let mut destinations = BTreeSet::new();
        let mut validate = |label: &str, file: &BundleFile| -> Result<()> {
            validate_relative(&file.source, "bundle source")?;
            validate_relative(&file.destination, "install destination")?;
            if file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                bail!("{label} has an invalid lowercase SHA-256 digest");
            }
            if file.length == 0 {
                bail!("{label} has an invalid zero byte length");
            }
            if !destinations.insert((file.root, normalize_relative(&file.destination))) {
                bail!(
                    "duplicate install destination for `{}`",
                    file.destination.display()
                );
            }
            Ok(())
        };
        validate("setup", &self.setup)?;
        if !self.setup.executable {
            bail!("setup maintenance payload must be marked executable");
        }
        for (component, manifest) in &self.components {
            for file in &manifest.files {
                validate(component.as_str(), file)?;
            }
            if matches!(
                component,
                Component::Agent | Component::Tray | Component::Cli
            ) && !manifest.files.is_empty()
                && !manifest.files.iter().any(|file| file.executable)
            {
                bail!("component `{component}` has no executable payload file");
            }
        }
        Ok(())
    }

    fn validate_macos_tray_bundle(&self) -> Result<()> {
        let files = &self
            .components
            .get(&Component::Tray)
            .expect("availability checked before macOS tray validation")
            .files;
        for (destination, executable) in [
            ("Contents/MacOS/gflick-tray", true),
            ("Contents/Info.plist", false),
            ("Contents/Resources/favicon.icns", false),
        ] {
            let matches = files.iter().any(|file| {
                file.root == InstallRoot::PrivateApp
                    && file.destination == Path::new(destination)
                    && file.executable == executable
            });
            if !matches {
                bail!(
                    "macOS tray payload must include `{destination}` below private_app with executable={executable}"
                );
            }
        }
        Ok(())
    }
}

pub fn validate_relative(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!(
            "{label} `{}` must be a non-empty relative path",
            path.display()
        );
    }
    if path
        .components()
        .any(|component| !matches!(component, PathComponent::Normal(_)))
    {
        bail!(
            "{label} `{}` contains parent, root, or current-directory traversal",
            path.display()
        );
    }
    Ok(())
}

fn normalize_relative(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            PathComponent::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstalledFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<Component>,
    pub root: InstallRoot,
    pub destination: PathBuf,
    pub length: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstallState {
    pub schema: u32,
    pub product_version: String,
    pub selected_components: BTreeSet<Component>,
    pub files: Vec<InstalledFile>,
    #[serde(default)]
    pub registrations: RegistrationState,
}

impl InstallState {
    pub fn validate(&self) -> Result<()> {
        if self.schema != STATE_SCHEMA {
            bail!("unsupported installed-state schema {}", self.schema);
        }
        if !self.selected_components.contains(&Component::Agent) {
            bail!("installed state is invalid because it omits required component `agent`");
        }
        if !self.files.iter().any(|file| file.component.is_none()) {
            bail!("installed state is invalid because the setup maintenance payload is missing");
        }
        let mut represented_components = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        for file in &self.files {
            validate_relative(&file.destination, "installed file destination")?;
            if !destinations.insert((file.root, normalize_relative(&file.destination))) {
                bail!(
                    "installed state repeats managed destination `{}`",
                    file.destination.display()
                );
            }
            if let Some(component) = file.component {
                if !self.selected_components.contains(&component) {
                    bail!("installed state contains a file for unselected component `{component}`");
                }
                represented_components.insert(component);
            }
        }
        for component in &self.selected_components {
            if !represented_components.contains(component) {
                bail!("installed state selects component `{component}` without any managed files");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_at(source: &str, root: InstallRoot, destination: &str, executable: bool) -> BundleFile {
        BundleFile {
            source: source.into(),
            root,
            destination: destination.into(),
            length: 1,
            sha256: "00".repeat(32),
            executable,
        }
    }

    fn file(source: &str, destination: &str) -> BundleFile {
        file_at(source, InstallRoot::PrivateBin, destination, true)
    }

    fn manifest() -> BundleManifest {
        let platform = Platform::current().unwrap();
        let tray_files = if platform == Platform::Macos {
            vec![
                file_at(
                    "payload/tray/gflick-tray",
                    InstallRoot::PrivateApp,
                    "Contents/MacOS/gflick-tray",
                    true,
                ),
                file_at(
                    "payload/tray/Info.plist",
                    InstallRoot::PrivateApp,
                    "Contents/Info.plist",
                    false,
                ),
                file_at(
                    "payload/tray/favicon.icns",
                    InstallRoot::PrivateApp,
                    "Contents/Resources/favicon.icns",
                    false,
                ),
            ]
        } else {
            vec![file("payload/gflick-tray.exe", "gflick-tray.exe")]
        };
        BundleManifest {
            schema: BUNDLE_SCHEMA,
            product_version: "1.0.0".into(),
            platform,
            arch: Architecture::current().unwrap(),
            required: BTreeSet::from([Component::Agent]),
            defaults: BTreeSet::from([Component::Agent, Component::Tray]),
            setup: file("gflick-setup.exe", "gflick-setup.exe"),
            components: BTreeMap::from([
                (
                    Component::Agent,
                    ComponentManifest {
                        files: vec![file("payload/gflick-agent.exe", "gflick-agent.exe")],
                    },
                ),
                (Component::Tray, ComponentManifest { files: tray_files }),
                (
                    Component::Cli,
                    ComponentManifest {
                        files: vec![file("payload/gflick.exe", "gflick.exe")],
                    },
                ),
            ]),
        }
    }

    #[test]
    fn validates_current_and_future_defaults() {
        let current = manifest();
        current
            .validate_model(current.platform, current.arch)
            .unwrap();
        let mut future = current;
        future.components.insert(
            Component::Settings,
            ComponentManifest {
                files: vec![
                    BundleFile {
                        source: "payload/settings/GFlick".into(),
                        root: InstallRoot::UserApplications,
                        destination: "GFlick.app/Contents/MacOS/GFlick".into(),
                        length: 3,
                        sha256: "11".repeat(32),
                        executable: true,
                    },
                    BundleFile {
                        source: "payload/settings/icon.icns".into(),
                        root: InstallRoot::UserApplications,
                        destination: "GFlick.app/Contents/Resources/icon.icns".into(),
                        length: 3,
                        sha256: "22".repeat(32),
                        executable: false,
                    },
                ],
            },
        );
        future.defaults.insert(Component::Settings);
        future.validate_model(future.platform, future.arch).unwrap();
    }

    #[test]
    fn rejects_missing_agent_unavailable_settings_and_invalid_defaults() {
        let mut value = manifest();
        value.components.remove(&Component::Agent);
        assert!(value.validate_model(value.platform, value.arch).is_err());
        let value = manifest();
        assert!(
            value
                .validate_selection(&BTreeSet::from([Component::Agent, Component::Settings]))
                .is_err()
        );
        let mut value = manifest();
        value.defaults.insert(Component::Settings);
        assert!(value.validate_model(value.platform, value.arch).is_err());
        assert!(
            manifest()
                .validate_selection(&BTreeSet::from([Component::Tray]))
                .is_err()
        );
    }

    #[test]
    fn rejects_schema_platform_arch_paths_duplicates_and_metadata() {
        let base = manifest();
        let mut value = base.clone();
        value.schema = 2;
        assert!(value.validate_model(value.platform, value.arch).is_err());
        let mut value = base.clone();
        value.platform = if value.platform == Platform::Windows {
            Platform::Macos
        } else {
            Platform::Windows
        };
        assert!(value.validate_model(base.platform, base.arch).is_err());
        let mut value = base.clone();
        value.arch = if value.arch == Architecture::X86_64 {
            Architecture::Aarch64
        } else {
            Architecture::X86_64
        };
        assert!(value.validate_model(base.platform, base.arch).is_err());
        let mut value = base.clone();
        value.setup.source = "../setup".into();
        assert!(value.validate_model(base.platform, base.arch).is_err());
        let mut value = base.clone();
        value.setup.destination = std::env::current_dir().unwrap();
        assert!(value.validate_model(base.platform, base.arch).is_err());
        let mut value = base.clone();
        value.components.get_mut(&Component::Agent).unwrap().files[0].destination =
            value.setup.destination.clone();
        assert!(value.validate_model(base.platform, base.arch).is_err());
        let mut value = base;
        value.setup.sha256 = "ABC".into();
        assert!(value.validate_model(value.platform, value.arch).is_err());
        let mut value = manifest();
        value.setup.executable = false;
        assert!(value.validate_model(value.platform, value.arch).is_err());
        let mut value = manifest();
        value.components.get_mut(&Component::Agent).unwrap().files[0].executable = false;
        assert!(value.validate_model(value.platform, value.arch).is_err());
    }

    #[test]
    fn rejects_intel_macos_runtime_clearly() {
        let mut value = manifest();
        value.platform = Platform::Macos;
        value.arch = Architecture::X86_64;
        let error = value
            .validate_model(Platform::Macos, Architecture::X86_64)
            .unwrap_err();
        assert!(error.to_string().contains("Intel macOS is unsupported"));
    }

    #[test]
    fn macos_tray_requires_and_accepts_a_complete_app_bundle() {
        let mut value = manifest();
        value.platform = Platform::Macos;
        value.arch = Architecture::Aarch64;
        let tray = value.components.get_mut(&Component::Tray).unwrap();
        tray.files = vec![file_at(
            "payload/tray/gflick-tray",
            InstallRoot::PrivateApp,
            "Contents/MacOS/gflick-tray",
            true,
        )];
        assert!(
            value
                .validate_model(Platform::Macos, Architecture::Aarch64)
                .is_err()
        );

        let tray = value.components.get_mut(&Component::Tray).unwrap();
        tray.files.push(BundleFile {
            source: "payload/tray/Info.plist".into(),
            root: InstallRoot::PrivateApp,
            destination: "Contents/Info.plist".into(),
            length: 1,
            sha256: "11".repeat(32),
            executable: false,
        });
        tray.files.push(BundleFile {
            source: "payload/tray/favicon.icns".into(),
            root: InstallRoot::PrivateApp,
            destination: "Contents/Resources/favicon.icns".into(),
            length: 1,
            sha256: "22".repeat(32),
            executable: false,
        });
        value
            .validate_model(Platform::Macos, Architecture::Aarch64)
            .unwrap();
    }

    #[test]
    fn serde_rejects_unknown_component_and_root() {
        let json = serde_json::to_value(manifest()).unwrap();
        let mut unknown = json.clone();
        unknown["components"]["probe"] = unknown["components"]["agent"].clone();
        assert!(serde_json::from_value::<BundleManifest>(unknown).is_err());
        let mut unknown = json;
        unknown["setup"]["root"] = serde_json::json!("anywhere");
        assert!(serde_json::from_value::<BundleManifest>(unknown).is_err());
    }

    #[test]
    fn installed_state_enforces_payload_component_and_destination_invariants() {
        let maintenance = InstalledFile {
            component: None,
            root: InstallRoot::PrivateBin,
            destination: "gflick-setup".into(),
            length: 1,
            sha256: "00".repeat(32),
            executable: true,
        };
        let agent = InstalledFile {
            component: Some(Component::Agent),
            root: InstallRoot::PrivateBin,
            destination: "gflick-agent".into(),
            length: 1,
            sha256: "11".repeat(32),
            executable: true,
        };
        let valid = InstallState {
            schema: STATE_SCHEMA,
            product_version: "1.0.0".into(),
            selected_components: BTreeSet::from([Component::Agent]),
            files: vec![maintenance.clone(), agent.clone()],
            registrations: RegistrationState::default(),
        };
        valid.validate().unwrap();

        let mut missing_maintenance = valid.clone();
        missing_maintenance.files.remove(0);
        assert!(missing_maintenance.validate().is_err());

        let mut missing_selected = valid.clone();
        missing_selected
            .files
            .retain(|file| file.component.is_none());
        assert!(missing_selected.validate().is_err());

        let mut unselected = valid.clone();
        let mut tray = agent.clone();
        tray.component = Some(Component::Tray);
        tray.destination = "gflick-tray".into();
        unselected.files.push(tray);
        assert!(unselected.validate().is_err());

        let mut duplicate = valid;
        let mut repeated = maintenance;
        repeated.destination = "GFLICK-AGENT".into();
        duplicate.files.push(repeated);
        assert!(duplicate.validate().is_err());
    }
}
