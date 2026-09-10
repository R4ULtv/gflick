//! Preferences for the client, separate from the agent's per-mouse settings.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Preferences {
    pub confirm_apply: bool,
    pub confirm_discard: bool,
    pub confirm_profile_writes: bool,
    /// Whether the login items have been set to their defaults already. Both
    /// start on, and this is what stops a later "off" from being undone on the
    /// next launch.
    pub startup_defaults_applied: bool,
}
impl Preferences {
    pub fn path() -> Result<PathBuf> {
        Ok(directories::BaseDirs::new()
            .context("No user configuration directory")?
            .config_dir()
            .join("gflick/settings-client.json"))
    }
    pub fn load() -> Result<Self> {
        Self::load_at(&Self::path()?)
    }
    fn load_at(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Could not read app preferences"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self) -> Result<()> {
        self.save_at(&Self::path()?)
    }
    fn save_at(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Invalid preferences path")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        file.persist(path)
            .context("Could not save app preferences")?;
        Ok(())
    }
    pub fn needs_confirmation(&self, discard: bool, profile_write: bool) -> bool {
        if discard {
            self.confirm_discard
        } else {
            self.confirm_apply || (self.confirm_profile_writes && profile_write)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apply_and_discard_confirmations_are_independent() {
        for apply in [false, true] {
            for discard in [false, true] {
                for profiles in [false, true] {
                    let p = Preferences {
                        confirm_apply: apply,
                        confirm_discard: discard,
                        confirm_profile_writes: profiles,
                        ..Preferences::default()
                    };
                    for profile_write in [false, true] {
                        assert_eq!(p.needs_confirmation(true, profile_write), discard);
                        assert_eq!(
                            p.needs_confirmation(false, profile_write),
                            apply || (profiles && profile_write)
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn startup_defaults_are_applied_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        // Nothing saved yet: the login items are still to be turned on.
        assert!(
            !Preferences::load_at(&path)
                .unwrap()
                .startup_defaults_applied
        );
        let applied = Preferences {
            startup_defaults_applied: true,
            ..Preferences::default()
        };
        applied.save_at(&path).unwrap();
        assert!(
            Preferences::load_at(&path)
                .unwrap()
                .startup_defaults_applied
        );
    }
    #[test]
    fn preferences_round_trip_and_prompts_default_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let defaults = Preferences::load_at(&path).unwrap();
        assert!(!defaults.needs_confirmation(false, true));
        assert!(!defaults.needs_confirmation(true, false));
        let p = Preferences {
            confirm_profile_writes: true,
            confirm_discard: true,
            ..defaults
        };
        p.save_at(&path).unwrap();
        assert_eq!(Preferences::load_at(&path).unwrap(), p);
        assert!(p.needs_confirmation(false, true));
        assert!(!p.needs_confirmation(false, false));
        assert!(p.needs_confirmation(true, false));
        assert_eq!(
            serde_json::from_str::<Preferences>("{}").unwrap(),
            Preferences::default()
        );
    }
}
