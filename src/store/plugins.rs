//! The installed plugin ledger; code bytes stay in immutable generation directories.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{PluginManifest, PluginRecord, SourcePackageRecord};
use crate::error::{Error, Result};

use super::SurrealStore;

const COLUMNS: &str = "name, manifest, digest, generation, upstream, signed_by, <string>installed_at AS installed_at, <string>updated_at AS updated_at";

#[derive(Deserialize)]
struct Row {
    name: String,
    manifest: Value,
    digest: String,
    generation: String,
    upstream: Option<String>,
    #[serde(default)]
    signed_by: Option<String>,
    installed_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Row {
    fn into_record(self) -> Result<PluginRecord> {
        let manifest: PluginManifest = serde_json::from_value(self.manifest)
            .map_err(|e| Error::store_malformed(format!("plugin manifest: {e}")))?;
        Ok(PluginRecord {
            id: self.name,
            manifest,
            digest: self.digest,
            generation: self.generation,
            upstream: self.upstream,
            signed_by: self.signed_by,
            installed_at: self.installed_at,
            updated_at: self.updated_at,
        })
    }
}

impl SurrealStore {
    /// Who has ever owned this source ID through a plugin, including after its code was removed.
    pub async fn get_plugin_source_owner(&self, source: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct Owner {
            plugin: String,
        }
        let mut response = self.db.query("SELECT plugin FROM plugin_source_owner WHERE id = type::record('plugin_source_owner', $name)")
            .bind(("name", source.to_string())).await?;
        let mut owners: Vec<Owner> = super::take_rows(&mut response, 0)?;
        Ok(owners.pop().map(|owner| owner.plugin))
    }
    /// Remove the package and only the source-module installation rows in one transaction.
    /// Mined content, source cursors and drawers live in other tables and are not touched.
    pub async fn delete_plugin_with_sources(&self, id: &str, sources: &[String]) -> Result<()> {
        let mut sql = String::from("BEGIN TRANSACTION;");
        for index in 0..sources.len() {
            sql.push_str(&format!(
                " DELETE type::record('source_package', $source{index});"
            ));
        }
        sql.push_str(" DELETE type::record('plugin', $plugin_id); COMMIT TRANSACTION;");
        let mut query = self.db.query(sql).bind(("plugin_id", id.to_string()));
        for (index, source) in sources.iter().enumerate() {
            query = query.bind((format!("source{index}"), source.clone()));
        }
        super::checked(query.await?)?;
        Ok(())
    }
    /// Atomically switch a plugin release and every source module that points into its generation.
    ///
    /// The archive bytes are staged before this call; no reader sees mixed old/new module rows after a commit.
    pub async fn publish_plugin_with_sources(
        &self,
        plugin: &PluginRecord,
        sources: &[SourcePackageRecord],
        retired_sources: &[String],
    ) -> Result<()> {
        let mut sql = String::from(
            "BEGIN TRANSACTION; UPSERT type::record('plugin', $plugin_id) SET name = $plugin_id, manifest = $manifest, digest = $digest, generation = $generation, upstream = $upstream, signed_by = $signed_by, installed_at = <datetime>$installed_at, updated_at = <datetime>$updated_at;",
        );
        for index in 0..sources.len() {
            sql.push_str(&format!(" UPSERT type::record('source_package', $source{index}_name) SET name = $source{index}_name, state = $source{index}_state, digest = $source{index}_digest, manifest = $source{index}_manifest, installed_at = <datetime>$source{index}_installed, updated_at = <datetime>$source{index}_updated, origin = $source{index}_origin, registry = NONE, archive_digest = $digest, signed_by = $signed_by, plugin = $plugin_id, generation = $generation;"));
            sql.push_str(&format!(" UPSERT type::record('plugin_source_owner', $source{index}_name) SET source = $source{index}_name, plugin = $plugin_id;"));
        }
        for index in 0..retired_sources.len() {
            sql.push_str(&format!(
                " DELETE type::record('source_package', $retired{index});"
            ));
        }
        sql.push_str(" COMMIT TRANSACTION;");
        let mut query = self
            .db
            .query(sql)
            .bind(("plugin_id", plugin.id.clone()))
            .bind(("manifest", super::bindable(&plugin.manifest)?))
            .bind(("digest", plugin.digest.clone()))
            .bind(("generation", plugin.generation.clone()))
            .bind(("upstream", plugin.upstream.clone()))
            .bind(("signed_by", plugin.signed_by.clone()))
            .bind(("installed_at", super::stored(plugin.installed_at)))
            .bind(("updated_at", super::stored(plugin.updated_at)));
        for (index, source) in sources.iter().enumerate() {
            query = query
                .bind((format!("source{index}_name"), source.name.clone()))
                .bind((
                    format!("source{index}_state"),
                    super::bindable(&source.state)?,
                ))
                .bind((format!("source{index}_digest"), source.digest.clone()))
                .bind((
                    format!("source{index}_manifest"),
                    super::bindable(&source.manifest)?,
                ))
                .bind((
                    format!("source{index}_installed"),
                    super::stored(source.installed_at),
                ))
                .bind((
                    format!("source{index}_updated"),
                    super::stored(source.updated_at),
                ))
                .bind((
                    format!("source{index}_origin"),
                    super::bindable(&source.origin)?,
                ));
        }
        for (index, name) in retired_sources.iter().enumerate() {
            query = query.bind((format!("retired{index}"), name.clone()));
        }
        super::checked(query.await?)?;
        Ok(())
    }
    /// Read a plugin by its stable ID.
    pub async fn get_plugin(&self, name: &str) -> Result<Option<PluginRecord>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {COLUMNS} FROM plugin WHERE id = type::record('plugin', $name)"
            ))
            .bind(("name", name.to_string()))
            .await?;
        let mut rows: Vec<Row> = super::take_rows(&mut response, 0)?;
        rows.pop().map(Row::into_record).transpose()
    }

    /// Read all installed plugin records in stable order.
    pub async fn list_plugins(&self) -> Result<Vec<PluginRecord>> {
        let mut response = self
            .db
            .query(format!("SELECT {COLUMNS} FROM plugin ORDER BY name"))
            .await?;
        let rows: Vec<Row> = super::take_rows(&mut response, 0)?;
        rows.into_iter().map(Row::into_record).collect()
    }

    /// Publish a checked artifact generation to the installation ledger.
    pub async fn save_plugin(&self, record: &PluginRecord) -> Result<()> {
        self.db.query("UPSERT type::record('plugin', $name) SET name = $name, manifest = $manifest, digest = $digest, generation = $generation, upstream = $upstream, signed_by = $signed_by, installed_at = <datetime>$installed_at, updated_at = <datetime>$updated_at")
            .bind(("name", record.id.clone()))
            .bind(("manifest", super::bindable(&record.manifest)?))
            .bind(("digest", record.digest.clone()))
            .bind(("generation", record.generation.clone()))
            .bind(("upstream", record.upstream.clone()))
            .bind(("signed_by", record.signed_by.clone()))
            .bind(("installed_at", super::stored(record.installed_at)))
            .bind(("updated_at", super::stored(record.updated_at)))
            .await?.check()?;
        Ok(())
    }

    /// Remove only the plugin ledger; callers first check module/dependency state.
    pub async fn delete_plugin(&self, id: &str) -> Result<()> {
        self.db
            .query("DELETE type::record('plugin', $name)")
            .bind(("name", id.to_string()))
            .await?
            .check()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PluginInfo, SourceOrigin, SourcePackageState};

    #[tokio::test]
    async fn replacing_a_plugin_generation_keeps_one_installation_record() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let now = Utc::now();
        let mut record = PluginRecord {
            id: "example".to_string(),
            manifest: PluginManifest {
                format: 1,
                plugin: PluginInfo {
                    id: "example".to_string(),
                    version: "0.1.0".to_string(),
                    provider: "example".to_string(),
                    description: "Demo".to_string(),
                    repository: "https://github.com/example/example".to_string(),
                    license: "MIT".to_string(),
                },
                memcastle: ">=0.4".to_string(),
                dependencies: Vec::new(),
                shared_config_schema: None,
                authentication: None,
                modules: Vec::new(),
            },
            digest: "old".to_string(),
            generation: "example/old".to_string(),
            upstream: None,
            signed_by: None,
            installed_at: now,
            updated_at: now,
        };
        store.save_plugin(&record).await.unwrap();
        record.generation = "example/new".to_string();
        record.digest = "new".to_string();
        store.save_plugin(&record).await.unwrap();
        let found = store.get_plugin("example").await.unwrap().unwrap();
        assert_eq!(found.generation, "example/new");
        assert_eq!(found.installed_at, now);
        assert_eq!(store.list_plugins().await.unwrap().len(), 1);
        store.delete_plugin("example").await.unwrap();
        assert!(store.list_plugins().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn publishing_two_source_modules_switches_their_ownership_together() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let now = Utc::now();
        let plugin = PluginRecord {
            id: "example".to_string(),
            manifest: PluginManifest {
                format: 1,
                plugin: PluginInfo {
                    id: "example".to_string(),
                    version: "0.1.0".to_string(),
                    provider: "example".to_string(),
                    description: "Two sources".to_string(),
                    repository: "https://github.com/example/example".to_string(),
                    license: "MIT".to_string(),
                },
                memcastle: ">=0.4".to_string(),
                dependencies: Vec::new(),
                shared_config_schema: None,
                authentication: None,
                modules: Vec::new(),
            },
            digest: "a".repeat(64),
            generation: "a".repeat(64),
            upstream: None,
            signed_by: None,
            installed_at: now,
            updated_at: now,
        };
        let source = |name: &str| {
            SourcePackageRecord {
            name: name.to_string(), state: SourcePackageState::Installed,
            digest: "b".repeat(64),
            manifest: crate::source::manifest::parse(
                &format!("[source]\nname = '{name}'\nversion = '0.1.0'\ndescription = 'one'\n[compatibility]\ncontract = '0.4.0'\nmemcastle = '>=0.4'\n"), &[],
            ).unwrap(),
            installed_at: now, updated_at: now, origin: SourceOrigin::Plugin,
            registry: None, archive_digest: Some(plugin.digest.clone()), signed_by: None,
            plugin: Some(plugin.id.clone()), generation: Some(plugin.generation.clone()),
        }
        };
        store
            .publish_plugin_with_sources(&plugin, &[source("one"), source("two")], &[])
            .await
            .unwrap();
        assert_eq!(store.list_plugins().await.unwrap().len(), 1);
        for name in ["one", "two"] {
            let found = store.get_source_package(name).await.unwrap().unwrap();
            assert_eq!(found.plugin.as_deref(), Some("example"));
            assert_eq!(found.state, SourcePackageState::Installed);
        }
        store
            .publish_plugin_with_sources(&plugin, &[source("one")], &["two".to_string()])
            .await
            .unwrap();
        assert!(store.get_source_package("two").await.unwrap().is_none());
        assert!(store.get_source_package("one").await.unwrap().is_some());
    }
}
