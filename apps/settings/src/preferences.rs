//! The agent is the sole writer of the shared settings document.
use anyhow::{Result, bail};
pub use gflick_protocol::AppPreferences as Preferences;
use gflick_protocol::{RequestCommand, ResponseData};

pub trait PreferenceStore: Sized {
    fn load() -> Result<Self>;
    fn save(&self) -> Result<()>;
}
impl PreferenceStore for Preferences {
    fn load() -> Result<Self> {
        match gflick_client::request(RequestCommand::GetAppPreferences)? {
            ResponseData::AppPreferences { preferences } => Ok(preferences),
            _ => bail!("The agent did not return app preferences. Rebuild and restart the agent."),
        }
    }
    fn save(&self) -> Result<()> {
        match gflick_client::request(RequestCommand::SetAppPreferences {
            preferences: self.clone(),
        })? {
            ResponseData::Acknowledged => Ok(()),
            _ => bail!("The agent did not acknowledge app preferences"),
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
}
