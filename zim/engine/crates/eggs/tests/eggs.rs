//! Egg-importer tests: the legacy JSON shape (a faithful fixture),
//! the `zim.toml` round trip, the security gates, and the leniency
//! contract (warnings, never silent changes).

use zim_engine_domain::ParserKind;
use zim_engine_eggs::{
    import_legacy_json, import_zim_toml, to_zim_toml, EggImportError, ImportPolicy,
};

/// A realistic legacy egg fixture (the documented PTDL_v1 shape,
/// including the double-encoded `config.files` string, the
/// `features` string array with Pterodactyl's own misspelling, and
/// the v1 single-image fallback).
const LEGACY_EGG: &str = r##"{
    "_comment": "A test fixture shaped like the pterodactyl/eggs exports",
    "meta": {
        "version": "PTDL_v1",
        "update_url": null
    },
    "name": "Paper Minecraft",
    "description": "A high performance Minecraft server.",
    "author": "support@pterodactyl.io",
    "features": ["eula"],
    "docker_images": {
        "Java 21": "eclipse-temurin:21-jre",
        "Java 17": "eclipse-temurin:17-jre"
    },
    "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}",
    "config": {
        "files": "{\"server.properties\": {\"parser\": \"properties\", \"replace\": [{\"match\": \"server-port\", \"value\": \"{{SERVER_PORT}}\"}]}}",
        "startup": {
            "done": "Done ([0-9.]+s)! For help, type \"help\""
        },
        "stop": {
            "command": "stop",
            "signal": false
        }
    },
    "scripts": {
        "installation": {
            "script": "#!/bin/bash\ncd /mnt/server\ncurl -o server.jar ${DOWNLOAD_URL}\n",
            "container": "debian:12-slim",
            "entrypoint": "bash"
        }
    },
    "variables": [
        {
            "name": "Server Jar File",
            "description": "The jar to run",
            "env_variable": "SERVER_JARFILE",
            "default_value": "server.jar",
            "user_viewable": true,
            "user_editable": true,
            "rules": ["required", "regex:^[a-zA-Z0-9_-]+\\.jar$"]
        },
        {
            "name": "Download URL",
            "description": "Where the installer pulls the jar from",
            "env_variable": "DOWNLOAD_URL",
            "default_value": "https://example.com/server.jar",
            "user_viewable": false,
            "user_editable": false,
            "rules": ["required"]
        }
    ]
}"##;

#[test]
fn a_legacy_egg_imports_with_everything_preserved() {
    // Roadmap M9 acceptance criterion 1: variables, startup
    // template, install script, and config parser rules preserved.
    let report =
        import_legacy_json(LEGACY_EGG, &ImportPolicy::default()).expect("the fixture egg imports");
    assert!(
        report.warnings.is_empty(),
        "warnings: {:?}",
        report.warnings
    );
    let egg = &report.egg;

    assert_eq!(egg.name, "Paper Minecraft");
    assert_eq!(egg.author, "support@pterodactyl.io");
    assert!(egg.features.eula);
    assert!(!egg.features.privileged_install_scripts);

    // The labeled images survive.
    assert_eq!(egg.images.len(), 2);
    assert!(egg.images.iter().any(
        |image| image.label == "Java 21" && image.image.to_string() == "eclipse-temurin:21-jre"
    ));

    // The startup template is verbatim.
    assert_eq!(
        egg.startup.as_str(),
        "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}"
    );

    // The install script is verbatim.
    let install = egg.scripts.install.as_ref().expect("install script");
    assert_eq!(install.entrypoint, "bash");
    assert_eq!(install.container.to_string(), "debian:12-slim");
    assert!(install.script.contains("curl -o server.jar"));

    // The config parser rules are translated, not dropped.
    assert_eq!(egg.config.files.len(), 1);
    let rule = &egg.config.files[0];
    assert_eq!(rule.file, "server.properties");
    assert_eq!(rule.parser, ParserKind::Properties);
    assert_eq!(rule.replace.len(), 1);
    assert_eq!(rule.replace[0].r#match, "server-port");
    assert_eq!(rule.replace[0].value, "{{SERVER_PORT}}");
    assert_eq!(
        egg.config.startup_done.as_deref(),
        Some("Done ([0-9.]+s)! For help, type \"help\"")
    );
    let stop = egg.config.stop.as_ref().expect("stop config");
    assert_eq!(stop.command, "stop");
    assert!(!stop.signal);

    // The variables are preserved with their flags and rules.
    assert_eq!(egg.variables.len(), 2);
    let jarfile = egg
        .variables
        .iter()
        .find(|v| v.env_name == "SERVER_JARFILE")
        .expect("SERVER_JARFILE");
    assert!(jarfile.user_viewable);
    assert!(jarfile.user_editable);
    assert_eq!(
        jarfile.rules,
        vec![
            "required".to_owned(),
            "regex:^[a-zA-Z0-9_-]+\\.jar$".to_owned()
        ]
    );
    let download = egg
        .variables
        .iter()
        .find(|v| v.env_name == "DOWNLOAD_URL")
        .expect("DOWNLOAD_URL");
    assert!(!download.user_viewable);
    assert!(!download.user_editable);
}

#[test]
fn privileged_install_scripts_are_rejected_by_default() {
    let source = LEGACY_EGG.replace(
        "\"features\": [\"eula\"],",
        "\"features\": [\"eula\", \"priviledged_install_scripts\"],",
    );
    let err = import_legacy_json(&source, &ImportPolicy::default()).unwrap_err();
    assert!(matches!(err, EggImportError::PrivilegedInstallScripts));

    // The explicit opt-in admits it.
    let report = import_legacy_json(
        &source,
        &ImportPolicy {
            allow_privileged_install_scripts: true,
            allow_requires_root: false,
        },
    )
    .expect("the opt-in admits the egg");
    assert!(report.egg.features.privileged_install_scripts);
}

#[test]
fn requires_root_is_rejected_by_default() {
    let source = LEGACY_EGG.replace(
        "\"features\": [\"eula\"],",
        "\"features\": [\"requires_root\"],",
    );
    let err = import_legacy_json(&source, &ImportPolicy::default()).unwrap_err();
    assert!(matches!(err, EggImportError::RequiresRoot));

    let report = import_legacy_json(
        &source,
        &ImportPolicy {
            allow_privileged_install_scripts: false,
            allow_requires_root: true,
        },
    )
    .expect("the opt-in admits the egg");
    assert!(report.egg.features.requires_root);
}

#[test]
fn the_object_feature_form_maps_too() {
    // The canonical ZIM object form (the future importer input).
    let source = LEGACY_EGG.replace(
        "\"features\": [\"eula\"],",
        "\"features\": {\"eula\": true, \"privileged_install_scripts\": false},",
    );
    let report =
        import_legacy_json(&source, &ImportPolicy::default()).expect("the object form imports");
    assert!(report.egg.features.eula);
    assert!(!report.egg.features.privileged_install_scripts);
}

#[test]
fn unknown_fields_and_features_become_warnings_not_errors() {
    let source = LEGACY_EGG
        .replace(
            "\"_comment\": \"A test fixture shaped like the pterodactyl/eggs exports\",",
            "\"_comment\": \"...\",\n    \"something_new\": {\"nested\": true},",
        )
        .replace(
            "\"features\": [\"eula\"],",
            "\"features\": [\"eula\", \"holo\"],",
        );
    let report = import_legacy_json(&source, &ImportPolicy::default())
        .expect("leniency: the egg still imports");
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("unknown field \"something_new\"")));
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("unknown feature \"holo\"")));
}

#[test]
fn unresolved_placeholders_warn() {
    // A placeholder that is neither intrinsic nor declared.
    let source = LEGACY_EGG.replace(
        "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}",
        "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{MYSTERY}}",
    );
    let report = import_legacy_json(&source, &ImportPolicy::default()).unwrap();
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("\"MYSTERY\"") && w.contains("neither an intrinsic")));
}

#[test]
fn missing_name_or_startup_is_an_error() {
    let no_name = LEGACY_EGG.replace("\"name\": \"Paper Minecraft\",", "");
    assert!(matches!(
        import_legacy_json(&no_name, &ImportPolicy::default()).unwrap_err(),
        EggImportError::MissingName
    ));
    let no_startup = LEGACY_EGG.replace(
        "\"startup\": \"java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}\",",
        "",
    );
    assert!(matches!(
        import_legacy_json(&no_startup, &ImportPolicy::default()).unwrap_err(),
        EggImportError::MissingStartup
    ));
}

#[test]
fn the_v1_single_image_fallback_and_the_install_key_work() {
    // v1 eggs: `docker_image` (not the map) and `scripts.install`
    // (not `installation`).
    let source = LEGACY_EGG
        .replace(
            "\"docker_images\": {\n        \"Java 21\": \"eclipse-temurin:21-jre\",\n        \"Java 17\": \"eclipse-temurin:17-jre\"\n    },",
            "\"docker_image\": \"eclipse-temurin:21-jre\",",
        )
        .replace("\"installation\": {", "\"install\": {");
    let report = import_legacy_json(&source, &ImportPolicy::default()).unwrap();
    assert_eq!(report.egg.images.len(), 1);
    assert_eq!(report.egg.images[0].label, "default");
    assert!(report.egg.scripts.install.is_some());
}

#[test]
fn a_zim_toml_document_round_trips() {
    // Import the legacy fixture, export it as zim.toml, re-import
    // that: the egg's semantic content survives the round trip.
    let first = import_legacy_json(LEGACY_EGG, &ImportPolicy::default()).unwrap();
    let exported = to_zim_toml(&first.egg).expect("the export serializes");

    let second =
        import_zim_toml(&exported, &ImportPolicy::default()).expect("the export re-imports");
    assert!(
        second.warnings.is_empty(),
        "warnings: {:?}",
        second.warnings
    );

    let (a, b) = (&first.egg, &second.egg);
    assert_eq!(a.name, b.name);
    assert_eq!(a.description, b.description);
    assert_eq!(a.author, b.author);
    assert_eq!(a.startup.as_str(), b.startup.as_str());
    assert_eq!(a.features, b.features);
    assert_eq!(a.images, b.images);
    assert_eq!(a.scripts, b.scripts);
    assert_eq!(a.config, b.config);
    assert_eq!(
        a.variables
            .iter()
            .map(|v| (v.env_name.clone(), v.default_value.clone(), v.rules.clone()))
            .collect::<Vec<_>>(),
        b.variables
            .iter()
            .map(|v| (v.env_name.clone(), v.default_value.clone(), v.rules.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_bad_format_version_is_rejected() {
    let source = "[zim]\nformat = 99\n[egg]\nname = \"X\"\nstartup = \"run\"\n";
    let err = import_zim_toml(source, &ImportPolicy::default()).unwrap_err();
    assert!(err.to_string().contains("format version"));
}
