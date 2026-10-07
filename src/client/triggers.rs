//! The client side of the trigger routes (`/api/triggers`).

use crate::app::{TriggerChange, TriggerPatch, TriggerView, TriggersReload, TriggersReport};
use crate::domain::{CredentialRef, FireOutcome, TriggerMechanism};
use crate::error::Result;

use super::DaemonClient;

impl DaemonClient {
    /// Every configured trigger (`GET /api/triggers`).
    ///
    /// # Errors
    ///
    /// [`crate::Error::DaemonNotRunning`], or [`crate::Error::Remote`] if the daemon refuses.
    pub async fn list_triggers(&self) -> Result<TriggersReport> {
        self.send(self.http.get(self.api_url(&["triggers"], None)?))
            .await
    }

    /// One trigger (`GET /api/triggers/{name}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_triggers`]; an unknown name is a 404.
    pub async fn show_trigger(&self, name: &str) -> Result<TriggerView> {
        self.send(self.http.get(self.api_url(&["triggers", name], None)?))
            .await
    }

    /// Create a trigger or change one (`PUT /api/triggers/{name}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_triggers`]; an invalid definition is a 400, a trigger that cannot be enabled yet a 409.
    pub async fn set_trigger(&self, name: &str, patch: &TriggerPatch) -> Result<TriggerChange> {
        self.send(
            self.http
                .put(self.api_url(&["triggers", name], None)?)
                .json(patch),
        )
        .await
    }

    /// Enable or disable a trigger (`POST /api/triggers/{name}/enable|disable`).
    ///
    /// # Errors
    ///
    /// As for [`Self::set_trigger`].
    pub async fn set_trigger_enabled(&self, name: &str, enabled: bool) -> Result<TriggerChange> {
        let action = if enabled { "enable" } else { "disable" };
        self.send(
            self.http
                .post(self.api_url(&["triggers", name, action], None)?),
        )
        .await
    }

    /// Remove a trigger (`DELETE /api/triggers/{name}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_triggers`].
    pub async fn remove_trigger(&self, name: &str) -> Result<()> {
        let _: serde_json::Value = self
            .send(self.http.delete(self.api_url(&["triggers", name], None)?))
            .await?;
        Ok(())
    }

    /// Have the daemon read its configuration file again (`POST /api/triggers/reload`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_triggers`]; a file that does not validate is a 409.
    pub async fn reload_triggers(&self) -> Result<TriggersReload> {
        self.send(self.http.post(self.api_url(&["triggers", "reload"], None)?))
            .await
    }

    /// Ask for a run through a trigger now (`POST /api/triggers/{name}/fire`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_triggers`]; a disabled trigger, or a miner that cannot run, is a 409.
    pub async fn fire_trigger(&self, name: &str) -> Result<FireOutcome> {
        self.send(
            self.http
                .post(self.api_url(&["triggers", name, "fire"], None)?),
        )
        .await
    }
}

/// What `memcastle trigger set` was given, before it becomes a [`TriggerPatch`].
///
/// Plain strings, so the flag grammar lives here, where it is tested, and `main.rs` only forwards the flags.
#[derive(Debug, Default)]
pub struct TriggerSetFlags<'a> {
    /// `--miner`.
    pub miner: Option<&'a str>,
    /// `--type`.
    pub kind: Option<&'a str>,
    /// `--credential-env`.
    pub credential_env: Option<&'a str>,
    /// `--credential-file`.
    pub credential_file: Option<&'a str>,
    /// `--setting`, as `KEY=VALUE`.
    pub settings: &'a [String],
    /// `--unset-setting`.
    pub unset_settings: &'a [String],
    /// `--unset`.
    pub unset: &'a [String],
    /// `--enable`.
    pub enable: bool,
}

impl TriggerSetFlags<'_> {
    /// The patch these flags describe.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidInput`] naming the flag whose value is not `KEY=VALUE`, or a `--type` that is not a
    /// mechanism.
    pub fn into_patch(self) -> Result<TriggerPatch> {
        let kind = self
            .kind
            .map(|raw| {
                TriggerMechanism::parse(raw).ok_or_else(|| {
                    crate::Error::invalid_input(
                        "--type",
                        format!(
                            "`{raw}` is not a trigger type; use schedule, poll, webhook or watch"
                        ),
                    )
                })
            })
            .transpose()?;
        let mut patch = TriggerPatch {
            miner: self.miner.map(str::to_string),
            kind,
            enabled: self.enable.then_some(true),
            unset_settings: self.unset_settings.to_vec(),
            unset: self.unset.to_vec(),
            ..TriggerPatch::default()
        };
        patch.credential = match (self.credential_env, self.credential_file) {
            (Some(name), _) => Some(CredentialRef::Env {
                name: name.to_string(),
            }),
            (None, Some(path)) => Some(CredentialRef::File {
                path: path.to_string(),
            }),
            (None, None) => None,
        };
        for raw in self.settings {
            let (key, value) = raw
                .split_once('=')
                .filter(|(key, _)| !key.trim().is_empty())
                .ok_or_else(|| {
                    crate::Error::invalid_input(
                        "--setting",
                        format!("`{raw}` is not `KEY=VALUE`; for example `--setting every=1h`"),
                    )
                })?;
            // JSON when it parses as one (`true`), a string otherwise: `every=1d` and `path=/n` are strings, which is
            // what the settings that take one expect.
            patch.settings.insert(
                key.trim().to_string(),
                serde_json::from_str(value)
                    .unwrap_or_else(|_| serde_json::Value::String(value.to_string())),
            );
        }
        Ok(patch)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_flags_map_onto_the_patch_and_never_enable_on_their_own() {
        let settings = vec!["every=1d".to_string(), "recursive=false".to_string()];
        let patch = TriggerSetFlags {
            miner: Some("docs"),
            kind: Some("schedule"),
            settings: &settings,
            ..TriggerSetFlags::default()
        }
        .into_patch()
        .expect("patch");
        assert_eq!(patch.kind, Some(TriggerMechanism::Schedule));
        assert_eq!(
            patch.enabled, None,
            "writing a trigger down never switches it on"
        );
        assert_eq!(patch.settings["every"], json!("1d"));
        assert_eq!(patch.settings["recursive"], json!(false));
    }

    #[test]
    fn enable_is_the_only_way_to_ask_for_it_in_the_same_command() {
        let patch = TriggerSetFlags {
            enable: true,
            ..TriggerSetFlags::default()
        }
        .into_patch()
        .expect("patch");
        assert_eq!(patch.enabled, Some(true));
    }

    #[test]
    fn a_secret_reference_is_an_env_name_or_a_file_and_never_a_value() {
        let patch = TriggerSetFlags {
            credential_env: Some("HOOK_SECRET"),
            ..TriggerSetFlags::default()
        }
        .into_patch()
        .expect("patch");
        assert_eq!(
            patch.credential,
            Some(CredentialRef::Env {
                name: "HOOK_SECRET".to_string()
            })
        );
    }

    #[test]
    fn a_bad_type_or_a_pair_without_an_equals_sign_names_the_flag() {
        let error = TriggerSetFlags {
            kind: Some("cron"),
            ..TriggerSetFlags::default()
        }
        .into_patch()
        .expect_err("not a type");
        assert!(error.to_string().contains("--type"), "{error}");
        let settings = vec!["every".to_string()];
        let error = TriggerSetFlags {
            settings: &settings,
            ..TriggerSetFlags::default()
        }
        .into_patch()
        .expect_err("not a pair");
        assert!(error.to_string().contains("--setting"), "{error}");
    }
}
