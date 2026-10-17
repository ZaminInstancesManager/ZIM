//! ZIM engine binary entrypoint.
//!
//! Argument surface: `--data-dir DIR`, `--port N` (environment
//! overrides via [`zim_engine_main::EngineOptions::from_env`]); the
//! offline `migrate` subcommand (M10) runs the Pterodactyl Panel
//! importer and exits before the engine boots. Runs until SIGTERM /
//! Ctrl-C / a local engine-control shutdown (M27).

#![forbid(unsafe_code)]

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // M10: the offline migration importer — no engine boot, no
    // async runtime, exit with the importer's status.
    if argv.first().map(String::as_str) == Some("migrate") {
        let rest: Vec<String> = argv.into_iter().skip(1).collect();
        std::process::exit(zim_engine_migrate_pterodactyl::cli::run_cli(&rest));
    }

    let mut options = zim_engine_main::EngineOptions::default();
    let mut args = argv.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                if let Some(dir) = args.next() {
                    options.data_dir = dir.into();
                }
            }
            "--port" => {
                if let Some(port) = args.next().and_then(|p| p.parse().ok()) {
                    options.port = port;
                }
            }
            "--help" | "-h" => {
                println!(
                    "zim_engine — ZIM engine (Zamin Instances Manager)\n\n\
                     Usage: zim_engine [--data-dir DIR] [--port N]\n\
                            zim_engine migrate pterodactyl \
                             (--panel-mysql-dsn=DSN | --panel-fixture=PATH) \
                             --zim-db=PATH [--dry-run]\n\n\
                     Environment: ZIM_DATA_DIR, ZIM_PORT, \
                     ZIM_BOOTSTRAP_ADMIN_EMAIL, ZIM_BOOTSTRAP_ADMIN_PASSWORD"
                );
                std::process::exit(0);
            }
            // M27: the installed-engine version probe — the desktop
            // shell's About surface runs `zim_engine --version` next
            // to the running engine's /health version, so the
            // update channel's version-lag window (UPDATES.md §3)
            // is visible, not surprising.
            "--version" | "-V" => {
                println!("zim_engine {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => {
                eprintln!("zim_engine: unknown argument {other:?} (see --help)");
                std::process::exit(2);
            }
        }
    }
    let options = zim_engine_main::EngineOptions::from_env(options);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("building the async runtime");
    runtime.block_on(async move {
        if let Err(error) = zim_engine_main::run(options).await {
            eprintln!("zim_engine: fatal: {error}");
            std::process::exit(1);
        }
    });
}
