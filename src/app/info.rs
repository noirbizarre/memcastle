//! What the daemon reports about how it is configured, for `GET /api/config` (the dashboard's settings page).
//!
//! An explicit, non-secret view built field by field, never a serialisation of [`Config`]: a field added to the
//! configuration later (a token, an API key, a URL with credentials) is therefore not exposed until someone decides
//! it should be. It says what is in effect, so a UI never has to guess from the files on disk.

use serde::{Deserialize, Serialize};

use crate::assets::{AssetSource, Assets};
use crate::config::{Config, EmbeddingProvider};

use super::AppServices;

/// Where the runtime assets come from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetsInfo {
    /// `override`, `installed` or `embedded`.
    pub source: String,
    /// The directory the assets are read from; absent when only the embedded ones exist.
    pub root: Option<String>,
}

/// The web UI's state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebInfo {
    /// Whether the daemon serves `/ui` (`web.enable`).
    pub enabled: bool,
    /// Whether the dashboard's files were found; false means `/ui` answers a page that says how to install them.
    pub built: bool,
}

/// The job scheduler's limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobsInfo {
    /// How many jobs run at once.
    pub max_concurrency: usize,
    /// Configured limit for mine, embed and extract jobs (clamped to leave a short-job slot).
    #[serde(default)]
    pub background_concurrency: usize,
    /// Seconds shutdown waits for running jobs.
    pub drain_timeout_secs: u64,
    /// Seconds a running job's lease lasts without a heartbeat.
    pub lease_ttl_secs: u64,
}

/// A derived-data provider: what it is and, when it names one, the model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderInfo {
    /// `none`, `command`, `http` or (extraction only) `heuristic`.
    pub provider: String,
    /// The model name, when the configuration gives one.
    pub model: Option<String>,
}

/// Mining and deduplication settings that decide what a job does.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MiningInfo {
    /// Characters per drawer.
    pub chunk_chars: usize,
    /// Documents one mining job ingests.
    pub max_documents: usize,
    /// How many registries `source search` consults; their addresses are not reported.
    pub registries: usize,
    /// Whether deduplication is on.
    pub dedup_enabled: bool,
}

/// The configuration in effect, without any secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigReport {
    /// The listener's actual bound address.
    pub bind_addr: String,
    /// The served palace directory.
    pub palace_path: String,
    /// `embedded` or `remote`.
    pub backend: String,
    /// Directory or credential-free URL of the datastore.
    pub location: String,
    /// Whether a bearer token is required. The token itself is never reported.
    pub auth_enabled: bool,
    /// The dashboard.
    pub web: WebInfo,
    /// The runtime assets.
    pub assets: AssetsInfo,
    /// The scheduler.
    pub jobs: JobsInfo,
    /// Embeddings.
    pub embeddings: ProviderInfo,
    /// Entity extraction.
    pub extraction: ProviderInfo,
    /// Mining and deduplication.
    pub mining: MiningInfo,
}

impl ConfigReport {
    /// The report for `config` and the `assets` resolved from it.
    ///
    /// Only what a dashboard shows is copied: no token, no API key, no endpoint URL and no command line.
    #[must_use]
    pub fn from_config(config: &Config, assets: &Assets) -> Self {
        let (source, root) = match assets.source() {
            AssetSource::Override(dir) => ("override", Some(dir.display().to_string())),
            AssetSource::Installed(dir) => ("installed", Some(dir.display().to_string())),
            AssetSource::Embedded => ("embedded", None),
        };
        Self {
            auth_enabled: config.auth.enabled,
            web: WebInfo {
                enabled: config.web.enable,
                built: assets.web_is_built(),
            },
            assets: AssetsInfo {
                source: source.to_string(),
                root,
            },
            jobs: JobsInfo {
                max_concurrency: config.jobs.max_concurrency,
                background_concurrency: config.jobs.background_concurrency,
                drain_timeout_secs: config.jobs.drain_timeout_secs,
                lease_ttl_secs: config.jobs.lease_ttl_secs,
            },
            embeddings: ProviderInfo {
                provider: match config.embeddings.provider {
                    EmbeddingProvider::None => "none",
                    EmbeddingProvider::Command => "command",
                    EmbeddingProvider::Http => "http",
                }
                .to_string(),
                model: config.embeddings.model.clone(),
            },
            extraction: ProviderInfo {
                provider: config.extraction.provider.as_str().to_string(),
                model: config.extraction.model.clone(),
            },
            mining: MiningInfo {
                chunk_chars: config.mining.chunk_chars,
                max_documents: config.mining.max_documents,
                registries: config.mining.registries.len(),
                dedup_enabled: config.dedup.enabled,
            },
            ..Self::default()
        }
    }
}

impl AppServices {
    /// The configuration in effect, for the dashboard's settings page.
    ///
    /// Daemon information, not memory: like `status` it is never gated by a [`crate::domain::MemoryMode`], so a
    /// `read_only` or `disabled` session can still see how the daemon is set up. It carries no secret.
    #[must_use]
    pub fn config_report(&self) -> ConfigReport {
        ConfigReport {
            bind_addr: self.runtime.bind_addr.clone(),
            palace_path: self.runtime.palace_path.clone(),
            backend: self.runtime.backend.clone(),
            location: self.runtime.location.clone(),
            auth_enabled: self.auth_enabled(),
            ..self.runtime.config.clone()
        }
    }

    /// Whether the daemon serves the dashboard under `/ui`, which is what lets the authentication layer admit its
    /// static files.
    #[must_use]
    pub fn web_enabled(&self) -> bool {
        self.runtime.config.web.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Secret;

    #[test]
    fn the_report_never_contains_a_secret_or_an_endpoint() {
        let mut config = Config::default();
        config.auth.enabled = true;
        config.auth.token = Some(Secret::new("super-secret-token-value"));
        config.embeddings.api_key = Some(Secret::new("embedding-api-key-value"));
        config.embeddings.url = Some("http://user:pass@embeddings.internal/v1".into());
        config.extraction.api_key = Some(Secret::new("extraction-api-key-value"));
        config.mining.registries = vec!["https://user:pass@registry.internal/index.json".into()];
        let assets = Assets::resolve(None, &crate::assets::InstallSearch::default()).unwrap();

        let json = serde_json::to_string(&ConfigReport::from_config(&config, &assets)).unwrap();

        for leaked in [
            "super-secret-token-value",
            "embedding-api-key-value",
            "extraction-api-key-value",
            "embeddings.internal",
            "registry.internal",
            "user:pass",
        ] {
            assert!(!json.contains(leaked), "{leaked} leaked into {json}");
        }
        assert!(json.contains("\"auth_enabled\":true"), "{json}");
        assert!(json.contains("\"registries\":1"), "{json}");
    }

    #[test]
    fn the_report_says_whether_the_dashboard_is_enabled_and_built() {
        let mut config = Config::default();
        config.web.enable = true;
        let assets = Assets::resolve(None, &crate::assets::InstallSearch::default()).unwrap();

        let report = ConfigReport::from_config(&config, &assets);

        assert!(report.web.enabled);
        assert!(
            !report.web.built,
            "nothing is installed, so only the embedded page exists"
        );
        assert_eq!(report.assets.source, "embedded");
        assert_eq!(report.assets.root, None);
    }
}
