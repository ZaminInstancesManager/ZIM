//! Egg-parity tests (M17, no Docker): an imported Pterodactyl egg
//! becomes a GameServerSpec whose install plan, applied through the
//! file API, produces exactly the files a Minecraft server expects
//! (`eula.txt`, the rewritten `server.properties`).

use std::fs;

use zim_engine_files::Filesystem;
use zim_engine_workloads_minecraft::{
    apply_file_steps, build_install_plan, GameServerSpec, InstallStep, MinecraftIntegration,
    EULA_FILE,
};
use zim_engine_workloads_sdk::WorkloadIntegration;

/// A faithful Paper-shaped legacy egg (the same fixture shape the
/// eggs crate's own tests use, with the parser rules exercised).
const PAPER_EGG: &str = r##"{
    "meta": { "version": "PTDL_v1", "update_url": null },
    "name": "Paper Minecraft",
    "description": "A high performance Minecraft server.",
    "author": "support@pterodactyl.io",
    "docker_images": {
        "Java 21": "eclipse-temurin:21-jre",
        "Java 17": "eclipse-temurin:17-jre"
    },
    "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}",
    "config": {
        "files": "{\"server.properties\": {\"parser\": \"properties\", \"replace\": [{\"match\": \"server-port\", \"value\": \"{{SERVER_PORT}}\"}, {\"match\": \"motd\", \"value\": \"A {{MOTD_TEXT}} Server\"}]}}",
        "startup": { "done": "Done ([0-9.]+s)! For help, type \"help\"" },
        "stop": { "command": "stop", "signal": false }
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
            "name": "Server Memory",
            "description": "The heap ceiling in MB",
            "env_variable": "SERVER_MEMORY",
            "default_value": "512",
            "user_viewable": true,
            "user_editable": true,
            "rules": ["required", "regex:^[0-9]+$"]
        },
        {
            "name": "Server Port",
            "description": "The port to bind",
            "env_variable": "SERVER_PORT",
            "default_value": "25565",
            "user_viewable": true,
            "user_editable": true,
            "rules": ["required", "regex:^[0-9]+$"]
        },
        {
            "name": "MOTD Text",
            "description": "The message of the day",
            "env_variable": "MOTD_TEXT",
            "default_value": "ZIM",
            "user_viewable": true,
            "user_editable": true,
            "rules": []
        }
    ]
}"##;

fn paper_spec() -> GameServerSpec {
    let report =
        zim_engine_eggs::import_legacy_json(PAPER_EGG, &zim_engine_eggs::ImportPolicy::default())
            .expect("the fixture egg imports");
    let mut spec = GameServerSpec::from_egg(&report.egg, Some("Java 21")).expect("spec");
    spec.eula_accepted = true;
    spec.override_variable("MOTD_TEXT", "Pterodactyl-Parity")
        .unwrap();
    spec
}

fn tmp_fs() -> Filesystem {
    let root = std::env::temp_dir().join(format!("zim-mc-parity-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create temp root");
    Filesystem::new(&root)
}

#[test]
fn an_imported_paper_egg_instantiates() {
    let spec = paper_spec();
    spec.validate().expect("the paper spec validates");
    assert_eq!(spec.image, "eclipse-temurin:21-jre");
    assert_eq!(spec.stop.command, "stop");
    assert_eq!(spec.config_files.len(), 1);
    assert_eq!(spec.config_files[0].file, "server.properties");
    // The startup renders strictly with the egg's defaults.
    let rendered = spec.render_startup().expect("strict render");
    assert!(!rendered.contains("{{SERVER_MEMORY}}"));
    assert!(rendered.contains("-Xmx"));
}

#[test]
fn the_install_plan_has_all_three_step_kinds() {
    let spec = paper_spec();
    let plan = build_install_plan(&spec);
    assert!(plan
        .steps
        .iter()
        .any(|step| matches!(step, InstallStep::RunEggScript { container, .. } if container == "debian:12-slim")));
    assert!(plan
        .steps
        .iter()
        .any(|step| matches!(step, InstallStep::ApplyParserRule { file, .. } if file == "server.properties")));
    assert!(plan
        .steps
        .iter()
        .any(|step| matches!(step, InstallStep::WriteFile { path, .. } if path == EULA_FILE)));
    // Script first, files after (the installer may write the very
    // files the parser rules then rewrite).
    let script_index = plan
        .steps
        .iter()
        .position(|step| matches!(step, InstallStep::RunEggScript { .. }))
        .unwrap();
    let parser_index = plan
        .steps
        .iter()
        .position(|step| matches!(step, InstallStep::ApplyParserRule { .. }))
        .unwrap();
    assert!(script_index < parser_index);
}

#[test]
fn applying_the_file_steps_produces_the_minecraft_files() {
    let spec = paper_spec();
    let plan = build_install_plan(&spec);
    let fs_api = tmp_fs();

    // The install script "already ran": it left a server.properties
    // behind (what curl-based installers and shipped defaults do).
    fs_api
        .write_file(
            "server.properties",
            b"motd=A Minecraft Server\nonline-mode=true\nserver-port=1234\n",
        )
        .unwrap();

    let deferred = apply_file_steps(&plan, &fs_api).expect("apply file steps");
    assert_eq!(
        deferred, 1,
        "the egg script step is deferred to the job handler"
    );

    // The EULA file carries the canonical line.
    assert_eq!(fs_api.read_file(EULA_FILE).unwrap(), b"eula=true\n");

    // The parser rule rewrote the port (default 25565) and the motd
    // (the overridden variable), preserving unrelated keys.
    let properties = String::from_utf8(fs_api.read_file("server.properties").unwrap()).unwrap();
    assert!(properties.contains("server-port=25565"));
    assert!(properties.contains("motd=A Pterodactyl-Parity Server"));
    assert!(properties.contains("online-mode=true"));
    assert!(!properties.contains("server-port=1234"));
}

#[test]
fn skipping_egg_scripts_drops_the_script_step() {
    let mut spec = paper_spec();
    spec.skip_egg_scripts = true;
    let plan = build_install_plan(&spec);
    assert!(!plan
        .steps
        .iter()
        .any(|step| matches!(step, InstallStep::RunEggScript { .. })));
}

#[test]
fn an_installer_handled_eula_needs_no_explicit_write() {
    // The legacy `eula` feature flag: the egg's installer writes
    // eula.txt itself; the plan must not duplicate the file write
    // (a stale write could race the installer's own). The flag
    // arrives on the spec from the egg; the test sets it directly.
    let mut spec = paper_spec();
    assert!(!spec.eula_handled_by_installer);
    spec.eula_handled_by_installer = true;
    let plan = build_install_plan(&spec);
    assert!(!plan
        .steps
        .iter()
        .any(|step| matches!(step, InstallStep::WriteFile { path, .. } if path == EULA_FILE)));
}

#[test]
fn an_unaccepted_eula_writes_nothing() {
    let mut spec = paper_spec();
    spec.eula_accepted = false;
    let plan = build_install_plan(&spec);
    assert!(!plan
        .steps
        .iter()
        .any(|step| matches!(step, InstallStep::WriteFile { .. })));
}

#[test]
fn the_singleton_hooks_agree_with_the_typed_surface() {
    let spec = paper_spec();
    let value = serde_json::to_value(&spec).unwrap();

    // The engine-facing hook path: Value in, Value out.
    MinecraftIntegration
        .validate_spec(&value)
        .expect("validates");
    let plan_value = MinecraftIntegration
        .prepare_install(&value)
        .expect("prepare_install");
    let via_hook: serde_json::Value = plan_value;
    let direct = serde_json::to_value(build_install_plan(&spec)).unwrap();
    assert_eq!(via_hook, direct);
}
