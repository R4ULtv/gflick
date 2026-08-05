use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::manifest::{Architecture, BundleManifest, Platform};

#[derive(Clone, Debug)]
pub struct Bundle {
    root: PathBuf,
    manifest: BundleManifest,
}

#[derive(Clone, Debug)]
pub struct ValidatedBundle {
    root: PathBuf,
    manifest: BundleManifest,
}

impl Bundle {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize().with_context(|| {
            format!(
                "could not resolve bundle root `{}`",
                root.as_ref().display()
            )
        })?;
        let manifest_path = root.join("bundle.json");
        let bytes = fs::read(&manifest_path)
            .with_context(|| format!("could not read `{}`", manifest_path.display()))?;
        let manifest: BundleManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("could not parse `{}`", manifest_path.display()))?;
        Ok(Self { root, manifest })
    }

    #[must_use]
    pub fn manifest(&self) -> &BundleManifest {
        &self.manifest
    }

    pub fn validate(self) -> Result<ValidatedBundle> {
        let platform = Platform::current().context("unsupported runtime platform")?;
        let arch = Architecture::current().context("unsupported runtime architecture")?;
        self.validate_for(platform, arch)
    }

    pub fn validate_for(self, platform: Platform, arch: Architecture) -> Result<ValidatedBundle> {
        self.manifest.validate_model(platform, arch)?;
        for (_, file) in self
            .manifest
            .files_for(&self.manifest.components.keys().copied().collect())
        {
            let path = self.resolve_source(&file.source)?;
            let metadata = fs::metadata(&path)
                .with_context(|| format!("bundle payload `{}` is missing", path.display()))?;
            if !metadata.is_file() {
                bail!("bundle payload `{}` is not a regular file", path.display());
            }
            if metadata.len() != file.length {
                bail!(
                    "bundle payload `{}` has length {}, expected {}",
                    path.display(),
                    metadata.len(),
                    file.length
                );
            }
            let digest = digest_file(&path)?;
            if digest != file.sha256 {
                bail!(
                    "bundle payload `{}` failed SHA-256 validation",
                    path.display()
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let executable = metadata.permissions().mode() & 0o111 != 0;
                if executable != file.executable {
                    bail!(
                        "bundle payload `{}` executable metadata does not match the manifest",
                        path.display()
                    );
                }
            }
        }
        Ok(ValidatedBundle {
            root: self.root,
            manifest: self.manifest,
        })
    }

    fn resolve_source(&self, relative: &Path) -> Result<PathBuf> {
        let joined = self.root.join(relative);
        let resolved = joined
            .canonicalize()
            .with_context(|| format!("could not resolve bundle payload `{}`", joined.display()))?;
        if !resolved.starts_with(&self.root) {
            bail!(
                "bundle payload `{}` resolves outside the bundle root",
                relative.display()
            );
        }
        Ok(resolved)
    }
}

impl ValidatedBundle {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn manifest(&self) -> &BundleManifest {
        &self.manifest
    }

    pub fn source(&self, relative: &Path) -> Result<PathBuf> {
        let resolved = self.root.join(relative).canonicalize().with_context(|| {
            format!(
                "could not resolve verified payload `{}`",
                relative.display()
            )
        })?;
        if !resolved.starts_with(&self.root) {
            bail!("verified payload escaped the bundle root");
        }
        Ok(resolved)
    }
}

pub fn digest_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("could not open `{}` for hashing", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{BundleFile, Component, ComponentManifest, InstallRoot};
    use std::collections::{BTreeMap, BTreeSet};

    fn create_bundle() -> (tempfile::TempDir, BundleManifest) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("payload")).unwrap();
        fs::write(root.path().join("open-hub-setup.exe"), b"setup").unwrap();
        fs::write(root.path().join("payload/agent.exe"), b"agent").unwrap();
        fs::write(root.path().join("payload/tray.exe"), b"tray").unwrap();
        let make = |source: &str, destination: &str, bytes: &[u8]| BundleFile {
            source: source.into(),
            root: InstallRoot::PrivateBin,
            destination: destination.into(),
            length: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            executable: true,
        };
        let manifest = BundleManifest {
            schema: 1,
            product_version: "1.0.0".into(),
            platform: Platform::current().unwrap(),
            arch: Architecture::current().unwrap(),
            required: BTreeSet::from([Component::Agent]),
            defaults: BTreeSet::from([Component::Agent, Component::Tray]),
            setup: make("open-hub-setup.exe", "open-hub-setup.exe", b"setup"),
            components: BTreeMap::from([
                (
                    Component::Agent,
                    ComponentManifest {
                        files: vec![make("payload/agent.exe", "open-hub-agent.exe", b"agent")],
                    },
                ),
                (
                    Component::Tray,
                    ComponentManifest {
                        files: vec![make("payload/tray.exe", "open-hub-tray.exe", b"tray")],
                    },
                ),
            ]),
        };
        fs::write(
            root.path().join("bundle.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        (root, manifest)
    }

    #[test]
    fn verifies_every_payload_before_returning() {
        let (root, _) = create_bundle();
        Bundle::open(root.path()).unwrap().validate().unwrap();
    }

    #[test]
    fn rejects_missing_size_and_digest_mismatches() {
        let (root, manifest) = create_bundle();
        fs::remove_file(root.path().join("payload/tray.exe")).unwrap();
        assert!(Bundle::open(root.path()).unwrap().validate().is_err());
        fs::write(root.path().join("payload/tray.exe"), b"longer").unwrap();
        assert!(Bundle::open(root.path()).unwrap().validate().is_err());
        fs::write(root.path().join("payload/tray.exe"), b"TRAY").unwrap();
        assert!(Bundle::open(root.path()).unwrap().validate().is_err());
        assert_eq!(manifest.schema, 1);
    }
}
