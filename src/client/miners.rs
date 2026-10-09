//! The client side of the miner routes (`/api/miners`).

use crate::app::{MinerChange, MinerPatch, MinerView, MinersReload, MinersReport};
use crate::domain::Job;
use crate::error::Result;

use super::DaemonClient;

impl DaemonClient {
    /// Every configured miner (`GET /api/miners`).
    ///
    /// # Errors
    ///
    /// [`crate::Error::DaemonNotRunning`], or [`crate::Error::Remote`] if the daemon refuses.
    pub async fn list_miners(&self) -> Result<MinersReport> {
        self.send(self.http.get(self.api_url(&["miners"], None)?))
            .await
    }

    /// One miner (`GET /api/miners/{name}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_miners`]; an unknown name is a 404.
    pub async fn show_miner(&self, name: &str) -> Result<MinerView> {
        self.send(self.http.get(self.api_url(&["miners", name], None)?))
            .await
    }

    /// Create a miner or change one (`PUT /api/miners/{name}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_miners`]; an invalid definition is a 400, a scope that would widen a 409.
    pub async fn set_miner(&self, name: &str, patch: &MinerPatch) -> Result<MinerChange> {
        self.send(
            self.http
                .put(self.api_url(&["miners", name], None)?)
                .json(patch),
        )
        .await
    }

    /// Enable or disable a miner (`POST /api/miners/{name}/enable|disable`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_miners`].
    pub async fn set_miner_enabled(&self, name: &str, enabled: bool) -> Result<MinerChange> {
        let action = if enabled { "enable" } else { "disable" };
        self.send(
            self.http
                .post(self.api_url(&["miners", name, action], None)?),
        )
        .await
    }

    /// Remove a miner's definition (`DELETE /api/miners/{name}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_miners`].
    pub async fn remove_miner(&self, name: &str) -> Result<()> {
        let _: serde_json::Value = self
            .send(self.http.delete(self.api_url(&["miners", name], None)?))
            .await?;
        Ok(())
    }

    /// Have the daemon read its configuration file again (`POST /api/miners/reload`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_miners`]; a file that does not validate is a 409.
    pub async fn reload_miners(&self) -> Result<MinersReload> {
        self.send(self.http.post(self.api_url(&["miners", "reload"], None)?))
            .await
    }

    /// Submit a miner's mining job (`POST /api/miners/{name}/run`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_miners`]; a disabled or unrunnable miner is a 409.
    pub async fn run_miner(&self, name: &str, full: bool) -> Result<Job> {
        self.run_miner_with_options(name, full, &[], false).await
    }

    /// Parse one-off options like `mine`, resolving path values against the caller's directory.
    pub async fn run_miner_with_options(
        &self,
        name: &str,
        full: bool,
        words: &[String],
        allow_broaden: bool,
    ) -> Result<Job> {
        let parsed = super::mine::parse(name.to_string(), words.to_vec())?;
        if parsed.positional.is_some() {
            return Err(crate::Error::invalid_input(
                "options",
                "a miner already has a locator; give only KEY=VALUE options",
            ));
        }
        let mut options = parsed.options;
        if !options.is_empty() {
            let miner = self.show_miner(name).await?;
            let report = self.list_sources().await?;
            if let Some(adapter) = report
                .adapters
                .iter()
                .find(|adapter| adapter.name == miner.source)
            {
                super::mine::normalize_options(&mut options, &adapter.options)?;
            }
        }
        self.send(
            self.http
                .post(self.api_url(&["miners", name, "run"], None)?)
                .json(&serde_json::json!({
                    "full": full,
                    "options": options,
                    "allow_broaden": allow_broaden,
                    "requested_by": crate::domain::channel::CLI,
                })),
        )
        .await
    }
}

/// What `memcastle miner set` was given, before it becomes a [`MinerPatch`].
///
/// Plain strings, so the flag grammar lives here, where it is tested, and `main.rs` only forwards the flags.
#[derive(Debug, Default)]
pub struct SetFlags<'a> {
    /// `--source`.
    pub source: Option<&'a str>,
    /// `--locator`.
    pub locator: Option<&'a str>,
    /// `--wing`.
    pub wing: Option<&'a str>,
    /// `--credential-env`.
    pub credential_env: Option<&'a str>,
    /// `--credential-file`.
    pub credential_file: Option<&'a str>,
    /// `--credential-oauth`.
    pub credential_oauth: bool,
    /// `--option`, as `KEY=VALUE`.
    pub options: &'a [String],
    /// `--unset-option`.
    pub unset_options: &'a [String],
    /// `--unset`.
    pub unset: &'a [String],
    /// `--disabled`.
    pub disabled: bool,
    /// `--allow-broaden`.
    pub allow_broaden: bool,
}

/// `KEY=VALUE` split at the first `=`.
fn split_pair<'a>(flag: &str, raw: &'a str) -> Result<(&'a str, &'a str)> {
    raw.split_once('=')
        .filter(|(key, _)| !key.trim().is_empty())
        .ok_or_else(|| {
            crate::Error::invalid_input(
                flag,
                format!("`{raw}` is not `KEY=VALUE`; for example `{flag} key=value`"),
            )
        })
}

/// A setting's value: JSON when it parses as JSON (`3`, `true`, `["a"]`), a plain string otherwise.
fn setting_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

impl SetFlags<'_> {
    /// The patch these flags describe.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidInput`] naming the flag whose value is not `KEY=VALUE`.
    pub fn into_patch(self) -> Result<MinerPatch> {
        let mut patch = MinerPatch {
            source: self.source.map(str::to_string),
            locator: self.locator.map(str::to_string),
            wing: self.wing.map(str::to_string),
            enabled: self.disabled.then_some(false),
            allow_broaden: self.allow_broaden,
            unset_options: self.unset_options.to_vec(),
            unset: self.unset.to_vec(),
            ..MinerPatch::default()
        };
        patch.credential = match (self.credential_env, self.credential_file) {
            (Some(name), _) => Some(crate::domain::CredentialRef::Env {
                name: name.to_string(),
            }),
            (None, Some(path)) => Some(crate::domain::CredentialRef::File {
                path: path.to_string(),
            }),
            (None, None) => self
                .credential_oauth
                .then_some(crate::domain::CredentialRef::Oauth),
        };
        for raw in self.options {
            let (key, value) = split_pair("--option", raw)?;
            patch
                .options
                .insert(key.trim().to_string(), setting_value(value));
        }
        Ok(patch)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn options_are_json_when_they_parse_and_strings_otherwise() {
        let options = vec![
            "days=7".to_string(),
            "flag=true".to_string(),
            "name=alice".to_string(),
            "l=[\"a\"]".to_string(),
        ];
        let patch = SetFlags {
            options: &options,
            ..SetFlags::default()
        }
        .into_patch()
        .expect("patch");
        assert_eq!(patch.options["days"], json!(7));
        assert_eq!(patch.options["flag"], json!(true));
        assert_eq!(patch.options["name"], json!("alice"));
        assert_eq!(patch.options["l"], json!(["a"]));
    }

    #[test]
    fn a_pair_without_an_equals_sign_names_the_flag() {
        let options = vec!["groups".to_string()];
        let error = SetFlags {
            options: &options,
            ..SetFlags::default()
        }
        .into_patch()
        .expect_err("not a pair");
        assert!(error.to_string().contains("--option"), "{error}");
    }

    #[test]
    fn the_oauth_flag_is_a_credential_reference_that_names_nothing_secret() {
        let patch = SetFlags {
            credential_oauth: true,
            ..SetFlags::default()
        }
        .into_patch()
        .expect("patch");
        assert_eq!(patch.credential, Some(crate::domain::CredentialRef::Oauth));
        assert_eq!(
            serde_json::to_value(patch.credential).unwrap(),
            json!({"type": "oauth"})
        );
    }

    #[test]
    fn the_flags_map_onto_the_patch_without_inventing_anything() {
        let patch = SetFlags {
            source: Some("signal"),
            credential_env: Some("SIGNAL_TOKEN"),
            disabled: true,
            ..SetFlags::default()
        }
        .into_patch()
        .expect("patch");
        assert_eq!(patch.source.as_deref(), Some("signal"));
        assert_eq!(patch.enabled, Some(false));
        assert!(patch.locator.is_none() && patch.wing.is_none() && !patch.allow_broaden);
        // Nothing asked to change the enabled state means the patch leaves it alone.
        let untouched = SetFlags::default().into_patch().expect("patch");
        assert_eq!(untouched, MinerPatch::default());
    }
}
