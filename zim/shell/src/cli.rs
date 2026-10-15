//! Shell argument surface (mirrors the engine's std-only style —
//! no clap dependency, `--flag value` pairs, `--help` text kept
//! short and truthful).

#![forbid(unsafe_code)]

use std::path::PathBuf;

/// Parsed shell options.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The engine's data dir. `None` = the per-OS app-data default
    /// (`{app_data_dir}/engine`), resolved once the Tauri app exists
    /// (the app-data path needs the identifier).
    pub data_dir: Option<PathBuf>,
    /// Path to the `zim_engine` binary for spawn-when-absent.
    /// `None` = env `ZIM_ENGINE_PATH`, then a binary named
    /// `zim_engine` next to the shell's own executable (the M25
    /// sidecar layout), then `zim_engine` on the PATH.
    pub engine_path: Option<PathBuf>,
    /// A `zim://` URL to open once the engine is ready, instead of
    /// the default `zim://newtab` (session restore lands in M22;
    /// this flag is the manual hook for it today).
    pub open: Option<String>,
}

/// Parse `argv` (without the program name).
///
/// Unknown flags are errors, not silently ignored — a shell that
/// ignores `--data-d1r typos` is a shell that manages the wrong
/// engine.
pub fn parse(argv: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let Some(dir) = args.next() else {
                    return Err("--data-dir needs a directory path".to_owned());
                };
                options.data_dir = Some(PathBuf::from(dir));
            }
            "--engine-path" => {
                let Some(path) = args.next() else {
                    return Err("--engine-path needs a file path".to_owned());
                };
                options.engine_path = Some(PathBuf::from(path));
            }
            "--open" => {
                let Some(url) = args.next() else {
                    return Err("--open needs a zim:// URL".to_owned());
                };
                options.open = Some(url.to_owned());
            }
            "--help" | "-h" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            other => {
                return Err(format!("unknown argument {other:?} (see --help)"));
            }
        }
    }
    Ok(options)
}

/// The `--help` text.
pub const HELP: &str = "\
zim_shell — the ZIM desktop shell (Tauri 2)

Usage: zim_shell [--data-dir DIR] [--engine-path PATH] [--open URL]

The shell attaches to a running ZIM engine (loopback HTTPS) or
spawns a detached one, then opens the engine-served frontend at
zim://newtab. Closing the shell never stops the engine or any
server.

Options:
  --data-dir DIR     The engine's data dir (default: the shell's
                     per-OS app-data dir + /engine).
  --engine-path PATH The zim_engine binary to spawn when no engine
                     answers (default: ZIM_ENGINE_PATH env, then a
                     zim_engine next to this executable, then PATH).
  --open URL         A zim:// URL to open instead of zim://newtab.
";

/// Resolve the effective engine path: flag, then env, then the
/// sidecar sibling, then bare PATH lookup.
pub fn resolve_engine_path(flag: &Option<PathBuf>) -> PathBuf {
    if let Some(path) = flag {
        return path.clone();
    }
    if let Ok(path) = std::env::var("ZIM_ENGINE_PATH") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    // The M25 bundle layout: the engine sits next to the shell.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(engine_binary_name());
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from(engine_binary_name())
}

/// The engine binary's platform file name.
pub fn engine_binary_name() -> &'static str {
    if cfg!(windows) {
        "zim_engine.exe"
    } else {
        "zim_engine"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_argv_is_defaults() {
        let options = parse(&argv(&[])).unwrap();
        assert!(options.data_dir.is_none());
        assert!(options.engine_path.is_none());
        assert!(options.open.is_none());
    }

    #[test]
    fn flags_parse_in_order() {
        let options = parse(&argv(&[
            "--data-dir",
            "/tmp/zed",
            "--engine-path",
            "/tmp/bin/zim_engine",
            "--open",
            "zim://servers",
        ]))
        .unwrap();
        assert_eq!(options.data_dir.unwrap(), PathBuf::from("/tmp/zed"));
        assert_eq!(
            options.engine_path.unwrap(),
            PathBuf::from("/tmp/bin/zim_engine")
        );
        assert_eq!(options.open.as_deref(), Some("zim://servers"));
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(parse(&argv(&["--data-d1r"])).is_err());
    }

    #[test]
    fn missing_value_is_an_error() {
        assert!(parse(&argv(&["--data-dir"])).is_err());
        assert!(parse(&argv(&["--engine-path"])).is_err());
        assert!(parse(&argv(&["--open"])).is_err());
    }
}
