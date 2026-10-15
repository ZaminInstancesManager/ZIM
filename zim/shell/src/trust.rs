//! Trusting the engine's loopback certificate (M21).
//!
//! The engine serves loopback HTTPS with a **persistent**
//! self-signed certificate (`loopback-cert.pem` in its data dir —
//! generated once, reused across boots). The system WebView
//! (WebView2 / WKWebView / WebKitGTK) validates it against the
//! **OS trust store**, so the shell's job is a first-run
//! installation — and a re-installation when the certificate
//! rotates (data dir moved, cert regenerated) — of that one
//! certificate:
//!
//! - **Windows**: `certutil -f -addstore -user Root <pem>` — the
//!   per-user Root store; no elevation, WebView2 honors it.
//! - **macOS**: `security add-trusted-cert -r trustRoot -k
//!   login.keychain-db <pem>` — the user's login keychain; the OS
//!   asks for confirmation once.
//! - **Linux**: copy into `/usr/local/share/ca-certificates/` +
//!   `update-ca-certificates` — WebKitGTK reads the system store,
//!   and that store is root-owned, so an unprivileged shell reports
//!   `NeedsManualStep` with the two commands to run.
//!
//! Nothing anywhere disables TLS validation: the webview gets a
//! real, trusted certificate; the probe verifier in `engine.rs` is
//! a separate, loopback-dialed-only affair.
//!
//! Bookkeeping: `{state_dir}/trusted-cert.json` records the SHA-256
//! of the installed certificate, so re-runs are free and rotations
//! are detected.

#![forbid(unsafe_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The trust bookkeeping file name (in the shell's state dir).
const TRUSTED_RECORD: &str = "trusted-cert.json";

/// The bookkeeping record schema.
const TRUSTED_SCHEMA: u8 = 1;

/// The PEM copy the OS commands point at.
const CERT_COPY: &str = "engine-loopback.pem";

/// The certificate's platform file name inside the CA directory
/// (Linux only — the other platforms' commands reference the staged
/// PEM copy directly).
#[cfg(all(unix, not(target_os = "macos")))]
const CA_FILE_NAME: &str = "zim-engine-loopback.crt";

#[derive(Debug, Serialize, Deserialize)]
struct TrustedRecord {
    schema: u8,
    sha256: String,
}

/// The outcome of a trust check/installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustState {
    /// The recorded fingerprint matches — already trusted.
    Trusted,
    /// Installed now (first run or rotation) and recorded.
    InstalledNow,
    /// The platform needs an elevated manual step (Linux without
    /// root); the string is user-facing instructions.
    NeedsManualStep(String),
    /// The installation command failed; the string is the
    /// user-facing diagnostic.
    Failed(String),
}

/// SHA-256 of the PEM bytes (hex, lowercase).
pub fn fingerprint(pem: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pem.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Check (and if needed install) trust for the engine's certificate.
///
/// `state_dir` is the shell's per-OS app-data dir (where the PEM
/// copy and the bookkeeping record live). Idempotent: a matching
/// fingerprint short-circuits to [`TrustState::Trusted`].
pub fn ensure_trusted(cert_pem: &str, state_dir: &Path) -> TrustState {
    std::fs::create_dir_all(state_dir).ok();
    let fp = fingerprint(cert_pem);

    // Already recorded → already installed.
    if let Ok(body) = std::fs::read_to_string(state_dir.join(TRUSTED_RECORD)) {
        if let Ok(record) = serde_json::from_str::<TrustedRecord>(&body) {
            if record.schema == TRUSTED_SCHEMA && record.sha256 == fp {
                return TrustState::Trusted;
            }
        }
    }

    // Materialize the PEM copy the OS commands reference.
    let cert_path = state_dir.join(CERT_COPY);
    if let Err(e) = std::fs::write(&cert_path, cert_pem) {
        return TrustState::Failed(format!(
            "cannot stage the engine certificate at {}: {e}",
            cert_path.display()
        ));
    }

    let install = install_platform(&cert_path);

    // Record success so the next boot short-circuits.
    if matches!(install, TrustState::InstalledNow) {
        let record = TrustedRecord {
            schema: TRUSTED_SCHEMA,
            sha256: fp,
        };
        if let Ok(body) = serde_json::to_string(&record) {
            let _ = std::fs::write(state_dir.join(TRUSTED_RECORD), body);
        }
    }
    install
}

/// The per-platform installation command.
fn install_platform(cert_path: &Path) -> TrustState {
    #[cfg(windows)]
    {
        run_install(
            "certutil",
            &["-f", "-addstore", "-user", "Root"],
            cert_path,
            "the Windows per-user Root store",
        )
    }
    #[cfg(target_os = "macos")]
    {
        run_install(
            "security",
            &[
                "add-trusted-cert",
                "-r",
                "trustRoot",
                "-k",
                "login.keychain-db",
            ],
            cert_path,
            "the macOS login keychain",
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        install_linux(cert_path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = cert_path;
        TrustState::Failed("unsupported platform for trust installation".to_owned())
    }
}

/// Run `<program> [args…] <cert_path>` and classify the exit.
/// (Windows/macOS only — Linux's path is [`install_linux`].)
#[cfg(any(windows, target_os = "macos"))]
fn run_install(program: &str, args: &[&str], cert_path: &Path, target: &str) -> TrustState {
    let mut command = std::process::Command::new(program);
    command.args(args).arg(cert_path);
    match command.output() {
        Ok(output) if output.status.success() => TrustState::InstalledNow,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            TrustState::Failed(format!(
                "installing the engine certificate into {target} failed: {stderr}"
            ))
        }
        Err(e) => TrustState::Failed(format!(
            "cannot run {program} to install the certificate into {target}: {e}"
        )),
    }
}

/// Linux: WebKitGTK reads the system CA bundle; updating it needs
/// root, so an unprivileged shell surfaces the manual step.
#[cfg(all(unix, not(target_os = "macos")))]
fn install_linux(cert_path: &Path) -> TrustState {
    let ca_dir = Path::new("/usr/local/share/ca-certificates");
    let ca_file = ca_dir.join(CA_FILE_NAME);

    // Can we write the CA directory? (Root shells, containers, and
    // CI are the realistic yes-cases; a desktop user is a no.)
    if std::fs::create_dir_all(ca_dir).is_ok() && std::fs::copy(cert_path, &ca_file).is_ok() {
        let update = std::process::Command::new("update-ca-certificates").output();
        match update {
            Ok(output) if output.status.success() => return TrustState::InstalledNow,
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let _ = std::fs::remove_file(&ca_file);
                return TrustState::Failed(format!("update-ca-certificates failed: {stderr}"));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&ca_file);
                return TrustState::Failed(format!("cannot run update-ca-certificates: {e}"));
            }
        }
    }

    TrustState::NeedsManualStep(format!(
        "the engine's loopback certificate must be trusted once. Run:\n\
         \u{2022} sudo cp {cert} {ca}\n\
         \u{2022} sudo update-ca-certificates\n\
         then relaunch ZIM.",
        cert = cert_path.display(),
        ca = ca_file.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zim_shell_trust_{label}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fingerprint_is_stable_and_hex() {
        let fp = fingerprint("-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----\n");
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            fp,
            fingerprint("-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----\n")
        );
        assert_ne!(
            fp,
            fingerprint("-----BEGIN CERTIFICATE-----\ndef\n-----END CERTIFICATE-----\n")
        );
    }

    #[test]
    fn ensure_trusted_records_and_shortcircuits() {
        // On dev/CI machines the platform install may need elevation
        // (Linux) — the part under test here is the bookkeeping:
        // whatever the install returned, a second call with the
        // SAME pem must short-circuit to Trusted ONLY when the
        // install actually succeeded. For the manual-step path the
        // record must NOT be written (retrying after the user runs
        // the commands should re-attempt the install).
        let dir = scratch("record");
        let pem = "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----\n";

        let first = ensure_trusted(pem, &dir);
        match first {
            TrustState::InstalledNow => {
                // Install succeeded → the record exists and the
                // second call short-circuits.
                assert!(dir.join(TRUSTED_RECORD).exists());
                assert_eq!(ensure_trusted(pem, &dir), TrustState::Trusted);
            }
            TrustState::NeedsManualStep(_) | TrustState::Failed(_) => {
                // No record → a retry re-attempts the install.
                assert!(!dir.join(TRUSTED_RECORD).exists());
            }
            TrustState::Trusted => panic!("first run cannot short-circuit"),
        }

        // A rotated certificate must not read as trusted.
        if dir.join(TRUSTED_RECORD).exists() {
            let rotated = ensure_trusted(
                "-----BEGIN CERTIFICATE-----\nrotated\n-----END CERTIFICATE-----\n",
                &dir,
            );
            assert_ne!(rotated, TrustState::Trusted);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pem_copy_is_staged_for_the_os_commands() {
        // Even when the install path needs manual steps, the PEM
        // copy must exist for the instructions to reference.
        let dir = scratch("staged");
        let pem = "-----BEGIN CERTIFICATE-----\nstage\n-----END CERTIFICATE-----\n";
        let _ = ensure_trusted(pem, &dir);
        assert!(dir.join(CERT_COPY).exists());
        let staged = std::fs::read_to_string(dir.join(CERT_COPY)).unwrap();
        assert_eq!(staged, pem);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
