//! ZIM engine binary entrypoint.
//!
//! Minimal argument surface (full CLI arrives with the frontend
//! milestone): `--data-dir DIR`, `--port N`; environment overrides
//! via [`zim_engine_main::EngineOptions::from_env`]. Runs until
//! SIGTERM / Ctrl-C.

#![forbid(unsafe_code)]

fn main() {
    let mut options = zim_engine_main::EngineOptions::default();
    let mut args = std::env::args().skip(1);
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
                     Usage: zim_engine [--data-dir DIR] [--port N]\n\n\
                     Environment: ZIM_DATA_DIR, ZIM_PORT, \
                     ZIM_BOOTSTRAP_ADMIN_EMAIL, ZIM_BOOTSTRAP_ADMIN_PASSWORD"
                );
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
