//! One release exposing two independently activated WASM sources and an agent integration.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{TestDaemon, wait_for_job_status};
use memcastle::domain::{
    Job, JobStatus, PluginInfo, PluginManifest, PluginModule, PluginModuleKind, sha256_hex,
};
use memcastle::plugin::package;
use memcastle::source::{build::Project, scaffold};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn release(root: &Path) -> Vec<u8> {
    let mut files = BTreeMap::new();
    let mut modules = Vec::new();
    for name in ["first", "second"] {
        scaffold::init(root, name, scaffold::Template::Rust).unwrap();
        common::wasm::share_target_of(root, name);
        let project = Project::open(&root.join(name)).unwrap();
        let component = std::fs::read(project.build().unwrap()).unwrap();
        let manifest = std::fs::read(root.join(name).join("memcastle-source.toml")).unwrap();
        modules.push(PluginModule {
            id: name.to_string(),
            kind: PluginModuleKind::Source,
            version: "0.1.0".into(),
            manifest: format!("modules/{name}/memcastle-source.toml"),
            manifest_sha256: sha256_hex(&manifest),
            entry: format!("modules/{name}/source.wasm"),
            sha256: sha256_hex(&component),
            optional: false,
            contract: Some("0.4.0".into()),
            config_schema: None,
        });
        files.insert(format!("modules/{name}/memcastle-source.toml"), manifest);
        files.insert(format!("modules/{name}/source.wasm"), component);
    }
    let integration = b"format = 1\n[integration]\nid = 'assistant'\nversion = '0.1.0'\ndescription = 'agent-side lifecycle'\n[compatibility]\nmemcastle = '>=0.4'\n[agent]\nkind = 'opencode'\nentry = 'index.js'\n[[assets]]\nfrom = 'dist'\nto = '.'\n";
    let entry = b"export default { id: 'assistant', server: async () => ({}) };\n";
    modules.push(PluginModule {
        id: "assistant".into(),
        kind: PluginModuleKind::Integration,
        version: "0.1.0".into(),
        manifest: "modules/assistant/memcastle-integration.toml".into(),
        manifest_sha256: sha256_hex(integration),
        entry: "modules/assistant/dist/index.js".into(),
        sha256: sha256_hex(entry),
        optional: false,
        contract: None,
        config_schema: None,
    });
    files.insert(
        "modules/assistant/memcastle-integration.toml".into(),
        integration.to_vec(),
    );
    files.insert("modules/assistant/dist/index.js".into(), entry.to_vec());
    let manifest = PluginManifest {
        format: 1,
        plugin: PluginInfo {
            id: "example".into(),
            version: "0.1.0".into(),
            provider: "example".into(),
            description: "Two independent source kinds and an integration".into(),
            repository: "https://github.com/example/example".into(),
            license: "MIT".into(),
        },
        memcastle: ">=0.4".into(),
        dependencies: Vec::new(),
        shared_config_schema: None,
        authentication: None,
        modules,
    };
    files.insert(
        "plugin.toml".into(),
        toml::to_string(&manifest).unwrap().into_bytes(),
    );
    package::pack(&files).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn multiple_sources_and_an_integration_keep_independent_lifecycles_and_mined_data() {
    let project = tempfile::tempdir().unwrap();
    let archive = release(project.path());
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let preview: Value = client
        .post(format!("{}/api/plugins/preview", daemon.base_url))
        .body(archive.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["modules"].as_array().unwrap().len(), 3);
    let consents: BTreeMap<String, String> = preview["source_consents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            (
                source["source"].as_str().unwrap().to_string(),
                source["digest"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(consents.len(), 2);
    let refused = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .body(archive.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let raw = package::unpack(&archive).unwrap();
    let legacy_manifest =
        std::str::from_utf8(&raw.files["modules/first/memcastle-source.toml"]).unwrap();
    let legacy = memcastle::source::package::pack(
        legacy_manifest,
        &raw.files["modules/first/source.wasm"],
        &[],
    )
    .unwrap();
    let legacy_consent = consents.get("first").unwrap();
    let legacy_install = client
        .post(format!("{}/api/source-packages", daemon.base_url))
        .query(&[("consent", legacy_consent)])
        .body(legacy)
        .send()
        .await
        .unwrap();
    assert_eq!(legacy_install.status(), StatusCode::OK);
    let name_conflict = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .header(
            "x-memcastle-consents",
            serde_json::to_string(&consents).unwrap(),
        )
        .body(archive.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        name_conflict.status(),
        StatusCode::CONFLICT,
        "a plugin must not silently claim a legacy source"
    );
    let still_legacy: Value = client
        .get(format!("{}/api/source-packages/first", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(still_legacy["origin"], "package");
    let old_notes = tempfile::tempdir().unwrap();
    std::fs::write(
        old_notes.path().join("legacy.txt"),
        "The legacy avalanche remains in memory.\n",
    )
    .unwrap();
    client
        .post(format!(
            "{}/api/source-packages/first/enable",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap();
    let earlier: Job = client.post(format!("{}/api/jobs", daemon.base_url))
        .json(&json!({"type": "mine", "source": "first", "locator": old_notes.path(), "requested_by": "test"}))
        .send().await.unwrap().json().await.unwrap();
    wait_for_job_status(&client, &daemon.base_url, earlier.id, JobStatus::Completed).await;
    client
        .post(format!(
            "{}/api/source-packages/first/disable",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap();
    let removed_legacy = client
        .delete(format!("{}/api/source-packages/first", daemon.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(removed_legacy.status(), StatusCode::OK);
    let adoption: Value = client
        .post(format!("{}/api/plugins/preview", daemon.base_url))
        .body(archive.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(adoption["adoptions"], json!(["first"]));
    let without_adoption = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .header(
            "x-memcastle-consents",
            serde_json::to_string(&consents).unwrap(),
        )
        .body(archive.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(without_adoption.status(), StatusCode::CONFLICT);
    let installed = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .header(
            "x-memcastle-consents",
            serde_json::to_string(&consents).unwrap(),
        )
        .header("x-memcastle-adopt-sources", r#"["first"]"#)
        .body(archive.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        installed.status(),
        StatusCode::OK,
        "{}",
        installed.text().await.unwrap()
    );
    for name in ["first", "second"] {
        let response: Value = client
            .get(format!("{}/api/source-packages/{name}", daemon.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["state"], "installed", "{response}");
    }
    let refused_module_removal = client
        .delete(format!("{}/api/source-packages/second", daemon.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(refused_module_removal.status(), StatusCode::CONFLICT);
    let plugins_dir = daemon.palace_path.parent().unwrap().join("plugins");
    let catalogues = memcastle::integration::Catalog::plugin_catalogs(&plugins_dir).unwrap();
    assert_eq!(catalogues[0].integrations()[0].id(), "assistant");

    let activated: Value = client
        .post(format!(
            "{}/api/source-packages/first/enable",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(activated["state"], "enabled");
    let other: Value = client
        .get(format!("{}/api/source-packages/second", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["state"], "installed");
    let refused = client
        .delete(format!("{}/api/plugins/example", daemon.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);

    let notes = tempfile::tempdir().unwrap();
    std::fs::write(
        notes.path().join("memory.txt"),
        "The provider keeps an independent cursor.\n",
    )
    .unwrap();
    let submitted: Job = client.post(format!("{}/api/jobs", daemon.base_url))
        .json(&json!({"type": "mine", "source": "first", "locator": notes.path(), "requested_by": "test"}))
        .send().await.unwrap().json().await.unwrap();
    wait_for_job_status(
        &client,
        &daemon.base_url,
        submitted.id,
        JobStatus::Completed,
    )
    .await;
    client
        .post(format!(
            "{}/api/source-packages/first/disable",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap();
    let miner = client
        .put(format!("{}/api/miners/kept", daemon.base_url))
        .json(&json!({ "source": "second", "enabled": false }))
        .send()
        .await
        .unwrap();
    assert!(
        miner.status().is_success(),
        "{}",
        miner.text().await.unwrap()
    );
    let blocked = client
        .delete(format!("{}/api/plugins/example", daemon.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        blocked.status(),
        StatusCode::CONFLICT,
        "a disabled miner still holds its source configuration"
    );
    let deleted_miner = client
        .delete(format!("{}/api/miners/kept", daemon.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted_miner.status(), StatusCode::OK);
    let removed = client
        .delete(format!("{}/api/plugins/example", daemon.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        removed.status(),
        StatusCode::OK,
        "{}",
        removed.text().await.unwrap()
    );
    let hits: Vec<Value> = client
        .get(format!(
            "{}/api/search?q=independent+cursor",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !hits.is_empty(),
        "mined drawers must survive plugin removal"
    );
    let legacy_hits: Vec<Value> = client
        .get(format!("{}/api/search?q=legacy+avalanche", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !legacy_hits.is_empty(),
        "explicit adoption must retain legacy mined drawers"
    );
    let mut impostor = package::unpack(&archive).unwrap();
    impostor.manifest.plugin.id = "other".into();
    impostor.manifest.plugin.provider = "other".into();
    impostor.manifest.plugin.repository = "https://github.com/example/other".into();
    impostor.files.insert(
        "plugin.toml".into(),
        toml::to_string(&impostor.manifest).unwrap().into_bytes(),
    );
    let rejected = client
        .post(format!("{}/api/plugins/preview", daemon.base_url))
        .body(package::pack(&impostor.files).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(
        rejected.status(),
        StatusCode::CONFLICT,
        "uninstall must not let another plugin inherit retained source cursors"
    );
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_one_source_permissions_requires_its_own_consent_and_preserves_the_other_state() {
    let project = tempfile::tempdir().unwrap();
    let archive = release(project.path());
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let preview: Value = client
        .post(format!("{}/api/plugins/preview", daemon.base_url))
        .body(archive.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let consents: BTreeMap<String, String> = preview["source_consents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            (
                source["source"].as_str().unwrap().to_string(),
                source["digest"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let installed = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .header(
            "x-memcastle-consents",
            serde_json::to_string(&consents).unwrap(),
        )
        .body(archive.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(installed.status(), StatusCode::OK);
    client
        .post(format!(
            "{}/api/source-packages/first/enable",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(
        notes.path().join("retired.txt"),
        "The quartz horizon survives plugin updates.\n",
    )
    .unwrap();
    client
        .post(format!(
            "{}/api/source-packages/second/enable",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap();
    let submitted: Job = client.post(format!("{}/api/jobs", daemon.base_url))
        .json(&json!({"type": "mine", "source": "second", "locator": notes.path(), "requested_by": "test"}))
        .send().await.unwrap().json().await.unwrap();
    wait_for_job_status(
        &client,
        &daemon.base_url,
        submitted.id,
        JobStatus::Completed,
    )
    .await;
    client
        .post(format!(
            "{}/api/source-packages/second/disable",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap();

    let mut package = package::unpack(&archive).unwrap();
    package.manifest.plugin.version = "0.2.0".into();
    let name = "modules/first/memcastle-source.toml";
    let source = String::from_utf8(package.files[name].clone())
        .unwrap()
        .replace("version = \"0.1.0\"", "version = \"0.2.0\"")
        .replace(
            "[permissions.filesystem]",
            "[permissions]\nnetwork = true\n\n[permissions.filesystem]",
        );
    package.files.insert(name.into(), source.into_bytes());
    let first = package
        .manifest
        .modules
        .iter_mut()
        .find(|module| module.id == "first")
        .unwrap();
    first.version = "0.2.0".into();
    first.manifest_sha256 = sha256_hex(&package.files[name]);
    package.files.insert(
        "plugin.toml".into(),
        toml::to_string(&package.manifest).unwrap().into_bytes(),
    );
    let update = package::pack(&package.files).unwrap();

    let changed: Value = client
        .post(format!("{}/api/plugins/preview", daemon.base_url))
        .body(update.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first = changed["source_consents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["source"] == "first")
        .unwrap();
    assert_eq!(first["already_granted"], false);
    let second = changed["source_consents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["source"] == "second")
        .unwrap();
    assert_eq!(second["already_granted"], true);
    let refused = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .body(update.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let before: Value = client
        .get(format!("{}/api/plugins/example", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before["manifest"]["plugin"]["version"], "0.1.0");

    let changed_consent = serde_json::json!({ "first": first["digest"] });
    let upgraded = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .header("x-memcastle-consents", changed_consent.to_string())
        .body(update.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        upgraded.status(),
        StatusCode::OK,
        "{}",
        upgraded.text().await.unwrap()
    );
    let first: Value = client
        .get(format!("{}/api/source-packages/first", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second: Value = client
        .get(format!("{}/api/source-packages/second", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["state"], "enabled");
    assert_eq!(second["state"], "disabled");

    // Retiring a disabled source and an uninstalled integration must leave its mined data intact.
    let mut package = package::unpack(&update).unwrap();
    package.manifest.plugin.version = "0.3.0".into();
    package
        .manifest
        .modules
        .retain(|module| module.id == "first");
    package
        .files
        .retain(|path, _| path == "plugin.toml" || path.starts_with("modules/first/"));
    package.files.insert(
        "plugin.toml".into(),
        toml::to_string(&package.manifest).unwrap().into_bytes(),
    );
    let retired = package::pack(&package.files).unwrap();
    let response = client
        .post(format!("{}/api/plugins", daemon.base_url))
        .body(retired)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    let former = client
        .get(format!("{}/api/source-packages/second", daemon.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(former.status(), StatusCode::NOT_FOUND);
    let plugin: Value = client
        .get(format!("{}/api/plugins/example", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plugin["manifest"]["modules"].as_array().unwrap().len(), 1);
    let hits: Vec<Value> = client
        .get(format!("{}/api/search?q=quartz+horizon", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !hits.is_empty(),
        "retiring a source cannot delete what it mined"
    );
    daemon.shutdown().await;
}
