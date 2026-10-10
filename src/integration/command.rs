//! The four `memcastle integration` commands as one function from a request to the text to print.
//!
//! The binary only parses arguments and prints; everything it decides lives here, where it is tested without spawning a
//! process.

use std::collections::HashSet;
use std::path::Path;

use crate::assets::InstallSearch;
use crate::error::{Error, Result};
use crate::term::Painter;

use super::catalog::Catalog;
use super::install::{self, Context};
use super::render;

/// What was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Show every shipped integration and its state.
    List,
    /// Install the named integration.
    Install(String),
    /// Bring the named, already installed integration up to what is shipped.
    Update(String),
    /// Remove the named integration.
    Remove(String),
}

/// Run `operation` and return what to print.
///
/// `assets_dir` is the explicit assets root (flag, environment or config file, already merged by the caller), and
/// `search` is where an installed package would be. `json` selects the machine-readable form; otherwise the text is
/// coloured by `painter`. `remove` reads neither, so it works after the package that shipped the integration is gone.
///
/// # Errors
///
/// Those of the operation: see [`Catalog::open`], [`install::install`], [`install::update`], [`install::remove`].
pub fn execute(
    operation: &Operation,
    assets_dir: Option<&Path>,
    search: &InstallSearch,
    ctx: &Context<'_>,
    json: bool,
    painter: Painter,
) -> Result<String> {
    let outcome = match operation {
        Operation::List => {
            let catalog = Catalog::open(assets_dir, search)?;
            let report = render::list(&catalog, ctx)?;
            return if json {
                to_json(&report)
            } else {
                Ok(render::render_list(&report, painter))
            };
        }
        Operation::Install(id) => {
            let catalog = Catalog::open(assets_dir, search)?;
            install::install(catalog.get(id)?, &catalog, ctx)?
        }
        Operation::Update(id) => {
            let catalog = Catalog::open(assets_dir, search)?;
            install::update(catalog.get(id)?, &catalog, ctx)?
        }
        Operation::Remove(id) => install::remove(id, ctx)?,
    };
    if json {
        to_json(&outcome)
    } else {
        Ok(render::render_outcome(&outcome, painter))
    }
}

/// Resolve shipped integrations and the independently installed plugins' integration modules.
///
/// The plugin directory is local file state, not a daemon call: integration install/uninstall must remain possible
/// offline, and only the caller's selected module is registered with its agent.
///
/// # Errors
///
/// A missing integration, changed plugin asset or installation conflict.
pub fn execute_with_plugins(
    operation: &Operation,
    assets_dir: Option<&Path>,
    plugins_dir: &Path,
    search: &InstallSearch,
    ctx: &Context<'_>,
    json: bool,
    painter: Painter,
) -> Result<String> {
    if let Operation::Remove(_) = operation {
        return execute(operation, assets_dir, search, ctx, json, painter);
    }
    let plugin_catalogs = Catalog::plugin_catalogs(plugins_dir)?;
    let shipped = Catalog::open(assets_dir, search).or_else(|error| {
        if plugin_catalogs.is_empty() || !matches!(error, Error::IntegrationAssetsMissing { .. }) {
            return Err(error);
        }
        // An installation without Core-shipped integrations is still allowed to provide modules from plugins.
        Catalog::read(
            plugins_dir.to_path_buf(),
            crate::assets::AssetSource::Embedded,
        )
    })?;
    match operation {
        Operation::List => {
            let mut report = render::list(&shipped, ctx)?;
            let mut ids: HashSet<String> = report
                .integrations
                .iter()
                .map(|item| item.id.clone())
                .collect();
            for plugin in &plugin_catalogs {
                let listed = render::list(plugin, ctx)?;
                for item in listed.integrations {
                    if !ids.insert(item.id.clone()) {
                        return Err(Error::IntegrationConflict {
                            name: item.id.clone(),
                            path: plugin.root().display().to_string(),
                        });
                    }
                    report.integrations.push(item);
                }
                report.broken.extend(listed.broken);
            }
            report
                .integrations
                .sort_by(|left, right| left.id.cmp(&right.id));
            if json {
                to_json(&report)
            } else {
                Ok(render::render_list(&report, painter))
            }
        }
        Operation::Install(id) | Operation::Update(id) => {
            if shipped.integrations().iter().any(|entry| entry.id() == id)
                && plugin_catalogs
                    .iter()
                    .any(|catalog| catalog.integrations().iter().any(|entry| entry.id() == id))
            {
                return Err(Error::IntegrationConflict {
                    name: id.clone(),
                    path: format!("{} and an installed plugin", shipped.root().display()),
                });
            }
            let catalog = plugin_catalogs
                .iter()
                .find(|catalog| {
                    catalog
                        .integrations()
                        .iter()
                        .any(|shipped| shipped.id() == id)
                })
                .unwrap_or(&shipped);
            let outcome = match operation {
                Operation::Install(_) => install::install(catalog.get(id)?, catalog, ctx)?,
                Operation::Update(_) => install::update(catalog.get(id)?, catalog, ctx)?,
                _ => unreachable!("only install and update reach this branch"),
            };
            if json {
                to_json(&outcome)
            } else {
                Ok(render::render_outcome(&outcome, painter))
            }
        }
        Operation::Remove(_) => unreachable!("handled before looking for plugins"),
    }
}

fn to_json(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string_pretty(value)
        .map_err(|source| Error::serialization("the command's output", source))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::assets::AssetSource;
    use crate::integration::agent::{Locations, fake::FakeAgents};
    use crate::integration::manifest::MANIFEST_FILE;

    /// An assets root with a Pi integration, a machine to install onto, and the agents' stand-ins.
    struct World {
        root: tempfile::TempDir,
        fake: FakeAgents,
        locations: Locations,
    }

    impl World {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("assets/integrations/pi");
            std::fs::create_dir_all(dir.join("dist")).unwrap();
            std::fs::write(dir.join("dist/extension.js"), "export default 1\n").unwrap();
            std::fs::write(
                dir.join(MANIFEST_FILE),
                "format = 1\n[integration]\nid = \"pi\"\nversion = \"0.1.0\"\ndescription = \"pi\"\n\
                 [compatibility]\nmemcastle = \">=0.1\"\n[agent]\nkind = \"pi\"\nentry = \"extension.js\"\n\
                 [[assets]]\nfrom = \"dist\"\nto = \".\"\n",
            )
            .unwrap();
            let locations = Locations {
                agents_dir: root.path().join("data/agents"),
                opencode_config_dir: root.path().join("config/opencode"),
            };
            Self {
                root,
                fake: FakeAgents::new(),
                locations,
            }
        }

        fn run(&self, operation: &Operation, json: bool) -> Result<String> {
            let assets = self.root.path().join("assets");
            let ctx = Context::for_process(&self.locations, &self.fake);
            execute(
                operation,
                Some(&assets),
                &InstallSearch::default(),
                &ctx,
                json,
                Painter::PLAIN,
            )
        }
    }

    #[test]
    fn the_four_operations_run_in_order_and_each_prints_what_it_did() {
        let world = World::new();

        let listed = world.run(&Operation::List, false).unwrap();
        assert!(listed.contains("not installed"), "{listed}");
        let installed = world
            .run(&Operation::Install("pi".to_string()), false)
            .unwrap();
        assert!(installed.starts_with("Installed pi 0.1.0"), "{installed}");
        let unchanged = world
            .run(&Operation::Update("pi".to_string()), false)
            .unwrap();
        assert!(unchanged.contains("nothing changed"), "{unchanged}");
        let removed = world
            .run(&Operation::Remove("pi".to_string()), false)
            .unwrap();
        assert!(removed.starts_with("Removed pi 0.1.0"), "{removed}");
    }

    #[test]
    fn an_installed_plugin_integration_is_selected_and_registered_locally_without_a_daemon() {
        use crate::domain::{
            PluginInfo, PluginManifest, PluginModule, PluginModuleKind, sha256_hex,
        };
        let world = World::new();
        let manifest = b"format = 1\n[integration]\nid = 'assistant'\nversion = '0.1.0'\ndescription = 'assistant'\n[compatibility]\nmemcastle = '>=0.1'\n[agent]\nkind = 'pi'\nentry = 'extension.js'\n[[assets]]\nfrom = 'dist'\nto = '.'\n";
        let entry = b"export default 1\n";
        let plugin = PluginManifest {
            format: 1,
            plugin: PluginInfo {
                id: "example".into(),
                version: "0.1.0".into(),
                provider: "example".into(),
                description: "An integration".into(),
                repository: "https://github.com/example/example".into(),
                license: "MIT".into(),
            },
            memcastle: ">=0.1".into(),
            dependencies: Vec::new(),
            shared_config_schema: None,
            authentication: None,
            modules: vec![PluginModule {
                id: "assistant".into(),
                kind: PluginModuleKind::Integration,
                version: "0.1.0".into(),
                manifest: "modules/assistant/memcastle-integration.toml".into(),
                manifest_sha256: sha256_hex(manifest),
                entry: "modules/assistant/dist/extension.js".into(),
                sha256: sha256_hex(entry),
                optional: false,
                contract: None,
                config_schema: None,
            }],
        };
        let files = std::collections::BTreeMap::from([
            (
                "plugin.toml".to_string(),
                toml::to_string(&plugin).unwrap().into_bytes(),
            ),
            (
                "modules/assistant/memcastle-integration.toml".to_string(),
                manifest.to_vec(),
            ),
            (
                "modules/assistant/dist/extension.js".to_string(),
                entry.to_vec(),
            ),
        ]);
        let archive = crate::plugin::package::pack(&files).unwrap();
        let digest = sha256_hex(&archive);
        let plugins_dir = world.root.path().join("plugins");
        let generation = plugins_dir.join("example").join(&digest);
        let projected = generation.join("integrations/assistant");
        std::fs::create_dir_all(projected.join("dist")).unwrap();
        std::fs::write(generation.join("package.tar.gz"), archive).unwrap();
        std::fs::write(projected.join("memcastle-integration.toml"), manifest).unwrap();
        std::fs::write(projected.join("dist/extension.js"), entry).unwrap();
        std::fs::write(plugins_dir.join("example/current"), &digest).unwrap();
        let ctx = Context::for_process(&world.locations, &world.fake);
        let assets = world.root.path().join("assets");
        let listed = execute_with_plugins(
            &Operation::List,
            Some(&assets),
            &plugins_dir,
            &InstallSearch::default(),
            &ctx,
            true,
            Painter::PLAIN,
        )
        .unwrap();
        assert!(listed.contains("assistant"), "{listed}");
        let installed = execute_with_plugins(
            &Operation::Install("assistant".into()),
            Some(&assets),
            &plugins_dir,
            &InstallSearch::default(),
            &ctx,
            false,
            Painter::PLAIN,
        )
        .unwrap();
        assert!(installed.starts_with("Installed assistant"), "{installed}");
        assert!(
            world
                .locations
                .agents_dir
                .join("assistant/.memcastle-install.json")
                .is_file()
        );
        let removed = execute_with_plugins(
            &Operation::Remove("assistant".into()),
            Some(&assets),
            &plugins_dir,
            &InstallSearch::default(),
            &ctx,
            false,
            Painter::PLAIN,
        )
        .unwrap();
        assert!(removed.starts_with("Removed assistant"), "{removed}");
    }

    #[test]
    fn json_output_is_the_same_data_as_the_text_and_parses() {
        let world = World::new();

        let listed: serde_json::Value =
            serde_json::from_str(&world.run(&Operation::List, true).unwrap()).unwrap();
        let installed: serde_json::Value = serde_json::from_str(
            &world
                .run(&Operation::Install("pi".to_string()), true)
                .unwrap(),
        )
        .unwrap();

        assert_eq!(listed["integrations"][0]["id"], "pi");
        assert_eq!(listed["assets_source"], "override");
        assert_eq!(installed["action"], "installed");
    }

    #[test]
    fn remove_reads_no_assets_and_the_other_operations_say_what_is_wrong_with_them() {
        let world = World::new();
        world
            .run(&Operation::Install("pi".to_string()), false)
            .unwrap();
        std::fs::remove_dir_all(world.root.path().join("assets")).unwrap();

        assert!(
            world
                .run(&Operation::Remove("pi".to_string()), false)
                .is_ok()
        );
        assert!(matches!(
            world.run(&Operation::List, false).unwrap_err(),
            Error::AssetsNotFound { .. }
        ));
    }

    #[test]
    fn with_no_override_and_no_package_nothing_can_be_listed_installed_or_updated() {
        let world = World::new();
        let ctx = Context::for_process(&world.locations, &world.fake);
        let nowhere = |operation| {
            execute(
                &operation,
                None,
                &InstallSearch::default(),
                &ctx,
                false,
                Painter::PLAIN,
            )
        };

        for operation in [
            Operation::List,
            Operation::Install("pi".to_string()),
            Operation::Update("pi".to_string()),
        ] {
            assert!(matches!(
                nowhere(operation).unwrap_err(),
                Error::IntegrationAssetsMissing { .. }
            ));
        }
    }

    #[test]
    fn the_listing_names_an_installed_package_as_its_source() {
        let world = World::new();
        let share = world.root.path().join("prefix/share/memcastle");
        std::fs::create_dir_all(share.join("integrations")).unwrap();
        std::fs::create_dir_all(world.root.path().join("prefix/bin")).unwrap();
        let search = InstallSearch {
            exe_dir: Some(world.root.path().join("prefix/bin")),
            ..InstallSearch::default()
        };
        let ctx = Context::for_process(&world.locations, &world.fake);

        let listed = execute(&Operation::List, None, &search, &ctx, true, Painter::PLAIN).unwrap();

        assert!(
            listed.contains("\"assets_source\": \"installed\""),
            "{listed}"
        );
        let catalog = Catalog::open(None, &search).unwrap();
        assert_eq!(catalog.source(), &AssetSource::Installed(share));
    }
}
