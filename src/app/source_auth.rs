//! Signing mining sources in with OAuth (docs/adr/039).
//!
//! Like installing a source, this changes what the daemon may do with someone's account, so it is administrative: REST
//! and the CLI only, with no MCP tool, so an agent can never start a sign-in or widen what a source is allowed to read.
//! The sign-in itself, and the storing and renewing of its tokens, are `crate::credential`'s; what lives here is
//! finding the source, checking it declares a sign-in at all, and removing the credential with the source.

use std::time::Duration;

use crate::credential::{Challenge, FlowStatus};
use crate::domain::{OAuthRequirement, SourceAuth, SourcePackageRecord};
use crate::error::{Error, Result};
use crate::mining::AdapterInfo;

use super::AppServices;

/// The longest one `wait` call holds a request open, whatever the caller asks for: a long poll that is repeated, so
/// that no proxy or client timeout sits between the user and the end of their sign-in.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

impl AppServices {
    /// The installed source `name`, and what it declares to sign in with.
    ///
    /// Enabled or not: signing in before turning a source on is a reasonable order.
    async fn oauth_source(&self, name: &str) -> Result<(SourcePackageRecord, OAuthRequirement)> {
        let unsupported = || Error::CredentialOauthUnsupported {
            source_name: name.to_string(),
        };
        if crate::mining::registry::BUILTIN_NAMES.contains(&name) {
            return Err(unsupported());
        }
        let record = self.installed(name).await?;
        let requirement = record
            .manifest
            .permissions
            .oauth
            .clone()
            .ok_or_else(unsupported)?;
        Ok((record, requirement))
    }

    /// Start signing the installed source `name` in, and say what the user must do.
    ///
    /// Replaces a sign-in already in progress for the same source. Signing in again while already signed in is how a
    /// credential is replaced.
    ///
    /// # Errors
    ///
    /// [`Error::SourceNotFound`], [`Error::CredentialOauthUnsupported`] when the source declares no sign-in, and
    /// [`Error::CredentialFlowFailed`] when the provider cannot be reached.
    pub async fn begin_source_auth(&self, name: &str) -> Result<Challenge> {
        let (_, requirement) = self.oauth_source(name).await?;
        self.credentials.begin(name, &requirement).await
    }

    /// Wait for the sign-in `flow` of `name`: up to `window` (at most [`MAX_WAIT`]), after which it says it is still
    /// pending and the caller asks again.
    ///
    /// # Errors
    ///
    /// As [`Self::begin_source_auth`], and [`Error::CredentialFlowFailed`] when the sign-in ended badly.
    pub async fn wait_source_auth(
        &self,
        name: &str,
        flow: &str,
        window: Duration,
    ) -> Result<FlowStatus> {
        self.oauth_source(name).await?;
        self.credentials
            .wait(name, flow, window.min(MAX_WAIT))
            .await
    }

    /// Where `record`'s source stands on signing in, or `None` when it declares no sign-in.
    ///
    /// An unreadable store is not a reason to fail a listing, so it reads as signed out here; `source auth` and a
    /// run are where the store's error is reported.
    pub(super) async fn source_auth(&self, record: &SourcePackageRecord) -> Option<SourceAuth> {
        let requirement = record.manifest.permissions.oauth.clone()?;
        let credentials = self.credentials.clone();
        let name = record.name.clone();
        let asked = requirement.clone();
        let state = tokio::task::spawn_blocking(move || credentials.state(&name, &asked))
            .await
            .ok()
            .and_then(std::result::Result::ok);
        Some(SourceAuth {
            signed_in: state.as_ref().is_some_and(|state| state.signed_in),
            expires_at: state.as_ref().and_then(|state| state.expires_at),
            scopes: state.map(|state| state.scopes).unwrap_or_default(),
            provider: requirement.provider(),
            flows: [
                requirement.supports_device().then_some("device"),
                requirement.supports_browser().then_some("browser"),
            ]
            .into_iter()
            .flatten()
            .map(str::to_string)
            .collect(),
        })
    }

    /// `info` with where its source stands on signing in, when it declares a sign-in.
    pub(super) async fn with_sign_in(&self, mut info: AdapterInfo) -> AdapterInfo {
        if info.permissions.oauth.is_some()
            && let Ok(Some(record)) = self.store.get_source_package(&info.name).await
        {
            info.auth = self.source_auth(&record).await;
        }
        info
    }

    /// Forget the credential of `name`, as removing the source does.
    pub(super) async fn forget_source_credential(&self, name: &str) -> Result<()> {
        let credentials = self.credentials.clone();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || credentials.forget(&name))
            .await
            .map_err(|e| Error::CredentialStoreFailed {
                message: format!("forgetting the credential was interrupted: {e}"),
            })?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use super::*;
    use crate::config::MiningConfig;
    use crate::credential::fake::{self, Provider};
    use crate::credential::store::FileStore;
    use crate::credential::{Credentials, Timing};
    use crate::domain::{
        Compatibility, CredentialRef, ManifestSource, MinerDefinition, Permissions,
        SourceCapabilities, SourceManifest, SourcePackageState, sha256_hex,
    };

    struct Fixture {
        app: AppServices,
        provider: Arc<Provider>,
        dir: tempfile::TempDir,
    }

    fn manifest(name: &str, oauth: Option<OAuthRequirement>) -> SourceManifest {
        SourceManifest {
            format: 1,
            source: ManifestSource {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: "demo".to_string(),
                license: None,
                homepage: None,
                repository: None,
            },
            compatibility: Compatibility {
                contract: crate::domain::CONTRACT_VERSION.to_string(),
                memcastle: ">=0.1".to_string(),
            },
            capabilities: SourceCapabilities {
                needs_credentials: oauth.is_some(),
                ..SourceCapabilities::default()
            },
            permissions: Permissions {
                oauth,
                ..Permissions::default()
            },
            limits: Default::default(),
            options: Default::default(),
            triggers: Default::default(),
            build: None,
            test: None,
        }
    }

    async fn install(fixture: &Fixture, name: &str, oauth: Option<OAuthRequirement>) {
        let now = Utc::now();
        fixture
            .app
            .store
            .save_source_package(&SourcePackageRecord {
                name: name.to_string(),
                state: SourcePackageState::Enabled,
                digest: sha256_hex(b"component"),
                manifest: manifest(name, oauth),
                installed_at: now,
                updated_at: now,
                origin: crate::domain::SourceOrigin::Package,
                registry: None,
                archive_digest: None,
                signed_by: None,
            })
            .await
            .unwrap();
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let credentials = Credentials::with_timing(
            Box::new(FileStore::new(dir.path().join("credentials"))),
            Timing {
                min_poll: Duration::from_millis(10),
                slow_down_step: Duration::from_millis(10),
            },
        );
        let app = AppServices::for_tests()
            .await
            .with_credentials(credentials)
            .with_mining(MiningConfig {
                sources_dir: Some(dir.path().join("sources")),
                ..MiningConfig::default()
            });
        Fixture {
            app,
            provider: fake::start().await,
            dir,
        }
    }

    async fn sign_in(fixture: &Fixture, name: &str) {
        let challenge = fixture.app.begin_source_auth(name).await.unwrap();
        loop {
            match fixture
                .app
                .wait_source_auth(name, &challenge.flow, Duration::from_millis(100))
                .await
                .unwrap()
            {
                FlowStatus::SignedIn(_) => return,
                FlowStatus::Pending => {}
            }
        }
    }

    fn miner(source: &str, credential: Option<CredentialRef>) -> MinerDefinition {
        MinerDefinition {
            name: "m".to_string(),
            source: source.to_string(),
            enabled: true,
            locator: Some("x".to_string()),
            wing: None,
            credential,
            options: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn signing_in_a_source_that_declares_no_sign_in_or_does_not_exist_says_so() {
        let fixture = fixture().await;
        install(&fixture, "plain", None).await;

        let plain = fixture.app.begin_source_auth("plain").await.unwrap_err();
        assert!(
            matches!(plain, Error::CredentialOauthUnsupported { .. }),
            "{plain}"
        );
        let builtin = fixture
            .app
            .begin_source_auth("directory")
            .await
            .unwrap_err();
        assert!(
            matches!(builtin, Error::CredentialOauthUnsupported { .. }),
            "{builtin}"
        );
        let missing = fixture.app.begin_source_auth("nope").await.unwrap_err();
        assert!(matches!(missing, Error::SourceNotFound { .. }), "{missing}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_shows_whether_it_is_signed_in_in_its_description_and_in_the_list() {
        let fixture = fixture().await;
        install(&fixture, "demo", Some(fixture.provider.requirement())).await;
        install(&fixture, "plain", None).await;

        let before = fixture
            .app
            .show_source("demo", crate::domain::MemoryMode::Full)
            .await
            .unwrap();
        let auth = before
            .auth
            .expect("a source that signs in says where it stands");
        assert!(!auth.signed_in);
        assert_eq!(auth.flows, ["device", "browser"]);
        assert!(auth.provider.starts_with("127.0.0.1:"), "{}", auth.provider);

        sign_in(&fixture, "demo").await;

        let listed = fixture
            .app
            .list_sources(crate::domain::MemoryMode::Full)
            .await
            .unwrap()
            .adapters;
        let demo = listed.iter().find(|s| s.name == "demo").unwrap();
        assert!(demo.auth.as_ref().unwrap().signed_in);
        assert_eq!(demo.auth.as_ref().unwrap().scopes, ["read"]);
        assert!(
            listed
                .iter()
                .filter(|s| s.name != "demo")
                .all(|s| s.auth.is_none()),
            "a source that never signs in has nothing to say about it"
        );
        let shown = serde_json::to_string(&listed).unwrap();
        assert!(
            !shown.contains("refresh-") && !shown.contains("access-"),
            "{shown}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removing_a_source_removes_its_credential_so_a_namesake_does_not_inherit_it() {
        let fixture = fixture().await;
        install(&fixture, "demo", Some(fixture.provider.requirement())).await;
        sign_in(&fixture, "demo").await;
        let stored = fixture.dir.path().join("credentials/demo.json");
        assert!(stored.is_file());

        fixture.app.remove_source_package("demo").await.unwrap();

        assert!(!stored.exists());
        install(&fixture, "demo", Some(fixture.provider.requirement())).await;
        let again = fixture
            .app
            .show_source("demo", crate::domain::MemoryMode::Full)
            .await
            .unwrap();
        assert!(!again.auth.unwrap().signed_in);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_miner_for_a_source_that_signs_in_is_refused_until_it_is_signed_in_and_told_the_command()
     {
        let fixture = fixture().await;
        install(&fixture, "demo", Some(fixture.provider.requirement())).await;
        for credential in [None, Some(CredentialRef::Oauth)] {
            let reason = fixture
                .app
                .check_credential(&miner("demo", credential))
                .await
                .unwrap_err();
            assert!(reason.contains("memcastle source auth demo"), "{reason}");
        }

        sign_in(&fixture, "demo").await;
        for credential in [None, Some(CredentialRef::Oauth)] {
            fixture
                .app
                .check_credential(&miner("demo", credential))
                .await
                .unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_oauth_credential_on_a_miner_whose_source_does_not_sign_in_is_refused_and_says_what_to_use()
     {
        let fixture = fixture().await;
        install(&fixture, "plain", None).await;

        let reason = fixture
            .app
            .check_credential(&miner("plain", Some(CredentialRef::Oauth)))
            .await
            .unwrap_err();

        assert!(reason.contains("does not sign in with OAuth"), "{reason}");
        assert!(reason.contains("--credential-env"), "{reason}");
        fixture
            .app
            .check_credential(&miner("plain", None))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_replaced_by_a_newer_attempt_ends_with_an_answer_that_names_the_problem() {
        let fixture = fixture().await;
        install(&fixture, "demo", Some(fixture.provider.requirement())).await;
        fixture.provider.set(|b| b.pending_polls = 1_000);
        let first = fixture.app.begin_source_auth("demo").await.unwrap();
        let _second = fixture.app.begin_source_auth("demo").await.unwrap();

        let error = fixture
            .app
            .wait_source_auth("demo", &first.flow, Duration::from_millis(10))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("no such sign-in"), "{error}");
    }
}
