//! Installed source repository methods: the `source_package` table (docs/adr/026).
//!
//! The lifecycle and the manifest the user agreed to. The component itself is a file; `digest` ties the row to it.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{SourceManifest, SourceOrigin, SourcePackageRecord, SourcePackageState};
use crate::error::{Error, Result};

use super::SurrealStore;

/// The projection every package read shares.
const COLUMNS: &str = "name, state, digest, manifest, <string>installed_at AS installed_at, \
     <string>updated_at AS updated_at, origin, registry, archive_digest, signed_by";

/// A `source_package` row as stored.
#[derive(Deserialize)]
struct Row {
    name: String,
    state: SourcePackageState,
    digest: String,
    manifest: Value,
    installed_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    // Absent on a row written before registries existed (docs/adr/033), which was always a package from disk.
    #[serde(default)]
    origin: Option<SourceOrigin>,
    #[serde(default)]
    registry: Option<String>,
    #[serde(default)]
    archive_digest: Option<String>,
    #[serde(default)]
    signed_by: Option<String>,
}

impl Row {
    fn into_record(self) -> Result<SourcePackageRecord> {
        let manifest: SourceManifest = serde_json::from_value(self.manifest).map_err(|source| {
            Error::store_malformed(format!("source_package manifest: {source}"))
        })?;
        Ok(SourcePackageRecord {
            name: self.name,
            state: self.state,
            digest: self.digest,
            manifest,
            installed_at: self.installed_at,
            updated_at: self.updated_at,
            origin: self.origin.unwrap_or(SourceOrigin::Package),
            registry: self.registry,
            archive_digest: self.archive_digest,
            signed_by: self.signed_by,
        })
    }
}

impl SurrealStore {
    /// The installed source called `name`, or `None`.
    pub async fn get_source_package(&self, name: &str) -> Result<Option<SourcePackageRecord>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {COLUMNS} FROM source_package WHERE id = type::record('source_package', $name)"
            ))
            .bind(("name", name.to_string()))
            .await?;
        let mut rows: Vec<Row> = super::take_rows(&mut response, 0)?;
        rows.pop().map(Row::into_record).transpose()
    }

    /// Every installed source, by name.
    pub async fn list_source_packages(&self) -> Result<Vec<SourcePackageRecord>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {COLUMNS} FROM source_package ORDER BY name"
            ))
            .await?;
        let rows: Vec<Row> = super::take_rows(&mut response, 0)?;
        rows.into_iter().map(Row::into_record).collect()
    }

    /// Store `record`, replacing an earlier install of the same name (its `installed_at` is kept by the caller
    /// putting the original in the record).
    pub async fn save_source_package(&self, record: &SourcePackageRecord) -> Result<()> {
        self.db
            .query(
                "UPSERT type::record('source_package', $name) SET \
                 name = $name, state = $state, digest = $digest, manifest = $manifest, \
                 installed_at = <datetime>$installed_at, updated_at = <datetime>$updated_at, \
                 origin = $origin, registry = $registry, archive_digest = $archive_digest, \
                 signed_by = $signed_by",
            )
            .bind(("name", record.name.clone()))
            .bind(("state", super::bindable(&record.state)?))
            .bind(("digest", record.digest.clone()))
            .bind(("manifest", super::bindable(&record.manifest)?))
            .bind(("installed_at", super::stored(record.installed_at)))
            .bind(("updated_at", super::stored(record.updated_at)))
            .bind(("origin", super::bindable(&record.origin)?))
            .bind(("registry", record.registry.clone()))
            .bind(("archive_digest", record.archive_digest.clone()))
            .bind(("signed_by", record.signed_by.clone()))
            .await?
            .check()?;
        Ok(())
    }

    /// Change the stored state of `name`. Callers get the new state from `SourcePackageState::apply`.
    pub async fn set_source_package_state(
        &self,
        name: &str,
        state: SourcePackageState,
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE type::record('source_package', $name) SET state = $state, updated_at = <datetime>$at",
            )
            .bind(("name", name.to_string()))
            .bind(("state", super::bindable(&state)?))
            .bind(("at", super::stored(at)))
            .await?
            .check()?;
        Ok(())
    }

    /// Forget the installed source `name`. Its mined drawers and source records are untouched: they are history.
    pub async fn delete_source_package(&self, name: &str) -> Result<()> {
        self.db
            .query("DELETE type::record('source_package', $name)")
            .bind(("name", name.to_string()))
            .await?
            .check()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Compatibility, ManifestSource, SourceOrigin};

    fn record(name: &str) -> SourcePackageRecord {
        let now = Utc::now();
        SourcePackageRecord {
            name: name.to_string(),
            state: SourcePackageState::Installed,
            digest: "d1".into(),
            manifest: SourceManifest {
                format: 1,
                source: ManifestSource {
                    name: name.to_string(),
                    version: "0.1.0".into(),
                    description: "demo".into(),
                    license: None,
                    homepage: None,
                    repository: None,
                },
                compatibility: Compatibility {
                    contract: "0.3".into(),
                    memcastle: ">=0.1".into(),
                },
                capabilities: Default::default(),
                permissions: Default::default(),
                limits: Default::default(),
                build: None,
                test: None,
            },
            installed_at: now,
            updated_at: now,
            origin: SourceOrigin::Package,
            registry: None,
            archive_digest: None,
            signed_by: None,
        }
    }

    #[tokio::test]
    async fn an_installed_source_is_read_back_with_its_manifest_and_state() {
        let store = SurrealStore::connect_memory_for_tests().await;
        store.save_source_package(&record("demo")).await.unwrap();

        let found = store.get_source_package("demo").await.unwrap().unwrap();
        assert_eq!(found.manifest.source.name, "demo");
        assert_eq!(found.state, SourcePackageState::Installed);
        assert!(store.get_source_package("other").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn installing_the_same_name_again_replaces_the_row() {
        let store = SurrealStore::connect_memory_for_tests().await;
        store.save_source_package(&record("demo")).await.unwrap();
        let mut again = record("demo");
        again.digest = "d2".into();
        store.save_source_package(&again).await.unwrap();

        assert_eq!(store.list_source_packages().await.unwrap().len(), 1);
        assert_eq!(
            store
                .get_source_package("demo")
                .await
                .unwrap()
                .unwrap()
                .digest,
            "d2"
        );
    }

    #[tokio::test]
    async fn state_changes_and_removal_are_persisted() {
        let store = SurrealStore::connect_memory_for_tests().await;
        store.save_source_package(&record("demo")).await.unwrap();
        store
            .set_source_package_state("demo", SourcePackageState::Enabled, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            store
                .get_source_package("demo")
                .await
                .unwrap()
                .unwrap()
                .state,
            SourcePackageState::Enabled
        );

        store.delete_source_package("demo").await.unwrap();
        assert!(store.list_source_packages().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_row_whose_manifest_no_longer_parses_is_reported_as_malformed_not_skipped() {
        let store = SurrealStore::connect_memory_for_tests().await;
        store
            .db
            .query(
                "UPSERT type::record('source_package', 'broken') SET name = 'broken', state = 'enabled', \
                 digest = 'd', manifest = {}, installed_at = time::now(), updated_at = time::now()",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        let error = store
            .get_source_package("broken")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("source_package manifest"), "{error}");
        assert!(store.list_source_packages().await.is_err());
    }
}
