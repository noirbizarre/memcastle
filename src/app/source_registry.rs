//! Finding, installing and updating sources from registries (docs/adr/033).
//!
//! A source that ships with MemCastle is not here: it is installed from the start (docs/adr/040), so a registry never
//! installs, updates or replaces one.
//!
//! The same administrative rule as installing from a file (`source_packages`): REST and the CLI only, no MCP tool, so
//! an agent cannot reach out to a registry or install code. Everything an install checks is checked here the same way
//! (compatibility, consent to the permissions, the load proof); this module only adds *where the archive comes from*
//! and the verification that it is the one that was published.

use serde::{Deserialize, Serialize};

use crate::distribution::{Catalog, Registry, TrustPolicy};
use crate::domain::{IndexedVersion, Permissions, SourceOrigin, SourcePackageRecord, SourceState};
use crate::error::{Error, Result};
use crate::mining::registry::{BUILTIN_NAMES, describe_installed, installed_records};

use super::AppServices;
use super::source_packages::{InstalledSource, Upstream};

/// What is installed of a source a registry offers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    /// The installed version.
    pub version: String,
    /// Its state, `unavailable` included.
    pub state: SourceState,
}

/// One source a registry offers, as `memcastle source search` lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// The name to give to `source install`.
    pub name: String,
    /// What it reads.
    pub description: String,
    /// Where it was found: always a registry; kept so an answer still says what kind of source it describes.
    pub origin: SourceOrigin,
    /// The index that offers it.
    pub registry: String,
    /// The version `install` would take: the newest that is not yanked and runs on this MemCastle.
    pub version: Option<String>,
    /// Why there is no such version, when `version` is `None`.
    pub unavailable_reason: Option<String>,
    /// Its licence, when the index says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// What is installed, when something is.
    pub installed: Option<InstalledVersion>,
    /// Whether `version` is newer than what is installed.
    pub update_available: bool,
}

/// The answer to a search.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistrySearch {
    /// Every match, the first index to offer a name winning, as it would for an install.
    pub entries: Vec<RegistryEntry>,
    /// One line for each index that could not be read.
    pub warnings: Vec<String>,
}

/// What installing a source from a registry would do, which the user reviews before agreeing to its permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryPreview {
    /// The source.
    pub name: String,
    /// The version that would be installed.
    pub version: String,
    /// What it reads.
    pub description: String,
    /// Where it comes from: a registry.
    pub origin: SourceOrigin,
    /// The index it comes from.
    pub registry: String,
    /// The trusted key that signed it, when one did.
    pub signed_by: Option<String>,
    /// SHA-256 of the verified archive.
    pub archive_digest: String,
    /// What it asks for.
    pub permissions: Permissions,
    /// The digest that stands for agreeing to exactly those permissions.
    pub consent_digest: String,
    /// The installed version it would replace, when there is one.
    pub replaces: Option<String>,
}

/// Which source to install, from where.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryInstall {
    /// The source's name.
    pub name: String,
    /// A version; absent means the newest that can be installed.
    #[serde(default)]
    pub version: Option<String>,
    /// Consult only this index, instead of the configured ones.
    #[serde(default)]
    pub registry: Option<String>,
    /// The digest of the permissions the user agreed to.
    #[serde(default)]
    pub consent: Option<String>,
    /// Turn the source on once installed.
    #[serde(default)]
    pub enable: bool,
}

/// An installed source with a newer version available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCandidate {
    /// The source.
    pub name: String,
    /// The version installed.
    pub installed: String,
    /// The version an update would install.
    pub available: String,
    /// Where it comes from: a registry.
    pub origin: SourceOrigin,
}

/// The answer to checking for updates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheck {
    /// The sources that can be updated.
    pub updates: Vec<UpdateCandidate>,
    /// Indexes that could not be read, or that an installed source came from and is no longer configured.
    pub warnings: Vec<String>,
}

/// What an update did to one source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateStatus {
    /// It was updated.
    Updated,
    /// It is already the newest installable version.
    Current,
    /// The new version asks for permissions the installed one did not have, and nobody agreed to them yet.
    NeedsConsent {
        /// What it asks for, one line.
        permissions: String,
        /// The digest that agrees to exactly that.
        digest: String,
    },
    /// It could not be updated.
    Failed {
        /// Why.
        message: String,
    },
}

/// One source's update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateOutcome {
    /// The source.
    pub name: String,
    /// The version it was at.
    pub from: String,
    /// The version it was updated to, or would be.
    pub to: Option<String>,
    /// What happened.
    #[serde(flatten)]
    pub status: UpdateStatus,
}

fn running_version() -> semver::Version {
    // The package's own version is a literal semver, so this cannot fail; a failure would be a build error elsewhere.
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

/// The version of `entry`, or the lowest possible when the index (which was validated) somehow has none.
fn parsed(entry: &IndexedVersion) -> semver::Version {
    semver::Version::parse(&entry.version).unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

impl AppServices {
    /// Search the configured registries for sources whose name or description contains `query`.
    ///
    /// # Errors
    ///
    /// [`Error::SourceRegistryUnavailable`] when `registry` was chosen and cannot be read, and store errors.
    pub async fn search_registry(
        &self,
        query: Option<&str>,
        registry: Option<&str>,
    ) -> Result<RegistrySearch> {
        let catalog = Catalog::open(&self.mining, registry).await?;
        // What is installed includes what ships with MemCastle, so a registry that offers it says it is there.
        let installed = installed_records(&self.store, &self.mining).await?;
        let running = running_version();
        let mut search = RegistrySearch {
            warnings: catalog.warnings.clone(),
            ..RegistrySearch::default()
        };
        for registry in &catalog.registries {
            for source in registry.index.search(query.unwrap_or("")) {
                // Built-in names are never installed from anywhere, and the first index to offer a name is the one
                // an install would use, so a later one is not listed as if it were another choice.
                if BUILTIN_NAMES.contains(&source.name.as_str())
                    || search.entries.iter().any(|entry| entry.name == source.name)
                {
                    continue;
                }
                let (version, unavailable_reason) = match source.pick(None, &running) {
                    Ok(entry) => (Some(entry.version.clone()), None),
                    Err(reason) => (None, Some(reason)),
                };
                let record = installed.iter().find(|record| record.name == source.name);
                search.entries.push(RegistryEntry {
                    name: source.name.clone(),
                    description: source.description.clone(),
                    origin: SourceOrigin::Registry,
                    registry: registry.label.clone(),
                    // Only what came from a registry is updated from one: a bundled source is updated with MemCastle
                    // and a package from a file has no upstream, so offering either an update would be a lie.
                    update_available: match (&version, record) {
                        (Some(available), Some(record))
                            if record.origin == SourceOrigin::Registry =>
                        {
                            semver::Version::parse(available)
                                .and_then(|available| {
                                    semver::Version::parse(&record.manifest.source.version)
                                        .map(|current| available > current)
                                })
                                .unwrap_or(false)
                        }
                        _ => false,
                    },
                    version,
                    unavailable_reason,
                    license: source.license.clone(),
                    installed: record.map(|record| InstalledVersion {
                        version: record.manifest.source.version.clone(),
                        state: describe_installed(record, &self.mining).state,
                    }),
                });
            }
        }
        Ok(search)
    }

    /// Fetch and verify what `request` names, and report what installing it would do.
    ///
    /// Nothing is installed. The CLI shows the permissions from this, so the user agrees to what was actually
    /// downloaded and verified and not to what an index claimed.
    ///
    /// # Errors
    ///
    /// [`Error::SourceNotInRegistry`], [`Error::SourceRegistryUnavailable`], [`Error::SourceIntegrity`],
    /// [`Error::SourceUntrusted`] and the package errors of an install.
    pub async fn preview_registry_source(
        &self,
        name: &str,
        version: Option<&str>,
        registry: Option<&str>,
    ) -> Result<RegistryPreview> {
        let (fetched, upstream, entry, description) =
            self.fetch_from_registry(name, version, registry).await?;
        let package = Self::read_archive(fetched.archive).await?;
        crate::distribution::verify_identity(&package, name, &entry)?;
        crate::source::manifest::check_compatible(&package.manifest)?;
        let permissions = package.manifest.permissions.normalized();
        let replaces = self
            .store
            .get_source_package(name)
            .await?
            .map(|record| record.manifest.source.version);
        Ok(RegistryPreview {
            name: name.to_string(),
            version: entry.version,
            description,
            origin: upstream.origin,
            registry: upstream.registry.unwrap_or_default(),
            signed_by: upstream.signed_by,
            archive_digest: fetched.archive_digest,
            consent_digest: permissions.consent_digest(name),
            permissions,
            replaces,
        })
    }

    /// Install a source from the bundle or a registry.
    ///
    /// # Errors
    ///
    /// As [`Self::preview_registry_source`], and [`Error::SourceConsentRequired`] without the right consent.
    pub async fn install_registry_source(
        &self,
        request: RegistryInstall,
    ) -> Result<InstalledSource> {
        let (fetched, upstream, entry, _) = self
            .fetch_from_registry(
                &request.name,
                request.version.as_deref(),
                request.registry.as_deref(),
            )
            .await?;
        self.install_archive(
            fetched.archive,
            request.consent.as_deref(),
            request.enable,
            upstream,
            Some((&request.name, &entry)),
        )
        .await
    }

    /// The installed sources that a newer version exists for.
    ///
    /// A package installed from a file has no upstream and is never listed.
    ///
    /// # Errors
    ///
    /// Store errors. An index that cannot be read is a warning, not an error: it must not hide the other updates.
    pub async fn check_source_updates(&self) -> Result<UpdateCheck> {
        let catalog = Catalog::open(&self.mining, None).await?;
        let mut check = UpdateCheck {
            warnings: catalog.warnings.clone(),
            ..UpdateCheck::default()
        };
        for record in self.store.list_source_packages().await? {
            match self.update_target(&catalog, &record) {
                Ok(Some((_, entry))) => check.updates.push(UpdateCandidate {
                    name: record.name.clone(),
                    installed: record.manifest.source.version.clone(),
                    available: entry.version.clone(),
                    origin: record.origin,
                }),
                Ok(None) => {}
                Err(reason) => check.warnings.push(format!("{}: {reason}", record.name)),
            }
        }
        Ok(check)
    }

    /// Update `name`, or every source that has an update, to the newest version its origin offers.
    ///
    /// A new version that asks for permissions the installed one did not is not installed without `consent`: it comes
    /// back as [`UpdateStatus::NeedsConsent`] with the digest to agree to, so a source cannot widen its own reach by
    /// being updated. An update that asks for the same permissions needs nothing, since they were agreed to already.
    ///
    /// # Errors
    ///
    /// [`Error::SourceNotFound`] or [`Error::SourceBuiltin`] for a name that is not an installed package,
    /// [`Error::SourceBundled`] for a source that ships with MemCastle, [`Error::SourceNotInRegistry`] when `name` was
    /// installed from a file, and store errors. Failures of a
    /// particular update are outcomes, so one failure does not stop the others.
    pub async fn update_sources(
        &self,
        name: Option<&str>,
        consent: Option<&str>,
    ) -> Result<Vec<UpdateOutcome>> {
        let catalog = Catalog::open(&self.mining, None).await?;
        let records = match name {
            Some(name) => {
                let record = self.installed(name).await?;
                if record.origin == SourceOrigin::Bundled && self.is_bundled(name) {
                    return Err(Error::SourceBundled {
                        name: name.to_string(),
                        action: "updated".to_string(),
                    });
                }
                if record.origin != SourceOrigin::Registry {
                    return Err(Error::SourceNotInRegistry {
                        name: name.to_string(),
                        reason:
                            "it was installed from a file, so there is nothing to update it from; \
                                 install the new package with `memcastle source install <file>`"
                                .to_string(),
                    });
                }
                vec![record]
            }
            None => self.store.list_source_packages().await?,
        };

        let policy = TrustPolicy::from_config(&self.mining)?;
        let mut outcomes = Vec::new();
        for record in records {
            let from = record.manifest.source.version.clone();
            let outcome = |to: Option<String>, status| UpdateOutcome {
                name: record.name.clone(),
                from: from.clone(),
                to,
                status,
            };
            let (registry, entry) = match self.update_target(&catalog, &record) {
                Ok(Some(target)) => target,
                Ok(None) if name.is_some() => {
                    outcomes.push(outcome(None, UpdateStatus::Current));
                    continue;
                }
                Ok(None) => continue,
                Err(reason) if name.is_some() => {
                    outcomes.push(outcome(None, UpdateStatus::Failed { message: reason }));
                    continue;
                }
                // Sources an index no longer offers are what `check` reports; an update of everything leaves them.
                Err(_) => continue,
            };
            let to = Some(entry.version.clone());
            let status = match self
                .update_one(&record, registry, entry, &policy, consent)
                .await
            {
                Ok(()) => UpdateStatus::Updated,
                Err(Error::SourceConsentRequired {
                    permissions,
                    digest,
                    ..
                }) => UpdateStatus::NeedsConsent {
                    permissions,
                    digest,
                },
                Err(error) => UpdateStatus::Failed {
                    message: error.to_string(),
                },
            };
            outcomes.push(outcome(to, status));
        }
        Ok(outcomes)
    }

    /// What an update of `record` would install: the newest version of its origin's index, when it is newer.
    ///
    /// `Err` says why an update cannot be looked for, which is not the same as there being none.
    fn update_target<'a>(
        &self,
        catalog: &'a Catalog,
        record: &SourcePackageRecord,
    ) -> std::result::Result<Option<(&'a Registry, &'a IndexedVersion)>, String> {
        let registry = match record.origin {
            SourceOrigin::Registry => record
                .registry
                .as_deref()
                .and_then(|label| catalog.by_label(label)),
            // Built in, bundled (updated with MemCastle) or installed from a file: no registry to ask.
            SourceOrigin::Builtin | SourceOrigin::Bundled | SourceOrigin::Package => {
                return Ok(None);
            }
        };
        let Some(registry) = registry else {
            return Err(format!(
                "the registry it came from, {}, is not configured or cannot be read",
                record.registry.as_deref().unwrap_or("(unrecorded)")
            ));
        };
        let Some(source) = registry.index.find(&record.name) else {
            return Err(format!("{} no longer offers it", registry.label));
        };
        let Ok(entry) = source.pick(None, &running_version()) else {
            // Nothing installable (all yanked or incompatible) is "no update", and `search` says why.
            return Ok(None);
        };
        let installed = semver::Version::parse(&record.manifest.source.version)
            .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
        Ok((parsed(entry) > installed).then_some((registry, entry)))
    }

    /// Fetch, verify and install one update of `record`.
    async fn update_one(
        &self,
        record: &SourcePackageRecord,
        registry: &Registry,
        entry: &IndexedVersion,
        policy: &TrustPolicy,
        consent: Option<&str>,
    ) -> Result<()> {
        let fetched = registry.fetch(&record.name, entry, policy).await?;
        let package = Self::read_archive(fetched.archive.clone()).await?;
        // The user agreed to these permissions when they installed. A new version that asks for the same needs no new
        // agreement, and one that asks for anything else is installed only with the digest of exactly that.
        let agreed = record.manifest.permissions.consent_digest(&record.name);
        let wanted = package.manifest.permissions.consent_digest(&record.name);
        let implied = (agreed == wanted).then_some(wanted);
        let consent = implied.as_deref().or(consent);
        self.install_archive(
            fetched.archive,
            consent,
            false,
            Upstream {
                origin: SourceOrigin::Registry,
                registry: Some(registry.label.clone()),
                archive_digest: Some(fetched.archive_digest),
                signed_by: fetched.signed_by,
            },
            Some((&record.name, entry)),
        )
        .await?;
        Ok(())
    }

    /// Choose the index and version for `name`, then download and verify its archive.
    async fn fetch_from_registry(
        &self,
        name: &str,
        version: Option<&str>,
        registry: Option<&str>,
    ) -> Result<(
        crate::distribution::Fetched,
        Upstream,
        IndexedVersion,
        String,
    )> {
        if BUILTIN_NAMES.contains(&name) {
            return Err(Error::SourceBuiltin {
                name: name.to_string(),
            });
        }
        if !crate::domain::is_valid_source_name(name) {
            return Err(Error::invalid_input(
                "name",
                format!("`{name}` is not a source name: lowercase letters, digits and `-`"),
            ));
        }
        // It is installed already, from the release that carries it: a registry's copy would only be a second,
        // differently trusted one under the same name.
        if self.is_bundled(name) {
            return Err(Error::SourceBundled {
                name: name.to_string(),
                action: "installed from a registry".to_string(),
            });
        }
        let catalog = Catalog::open(&self.mining, registry).await?;
        let Some((found, source)) = catalog.locate(name) else {
            let searched = if catalog.registries.is_empty() {
                "no registry is configured; set `mining.registries`, or install a package file"
                    .to_string()
            } else {
                format!(
                    "none of {} offers it",
                    catalog
                        .registries
                        .iter()
                        .map(|registry| registry.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            return Err(Error::SourceNotInRegistry {
                name: name.to_string(),
                reason: if catalog.warnings.is_empty() {
                    searched
                } else {
                    format!("{searched} (unreadable: {})", catalog.warnings.join("; "))
                },
            });
        };
        let entry = source
            .pick(version, &running_version())
            .map_err(|reason| Error::SourceNotInRegistry {
                name: name.to_string(),
                reason,
            })?
            .clone();
        let description = source.description.clone();
        let policy = TrustPolicy::from_config(&self.mining)?;
        let fetched = found.fetch(name, &entry, &policy).await?;
        let upstream = Upstream {
            origin: SourceOrigin::Registry,
            registry: Some(found.label.clone()),
            archive_digest: Some(fetched.archive_digest.clone()),
            signed_by: fetched.signed_by.clone(),
        };
        Ok((fetched, upstream, entry, description))
    }
}
