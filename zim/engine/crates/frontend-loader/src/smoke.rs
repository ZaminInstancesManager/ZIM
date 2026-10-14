//! The activation smoke gate (§3.5).
//!
//! Every check the *engine* can execute before activating a package:
//! manifest schema, engine-version compatibility, API generation,
//! signature classification, entrypoint/asset presence, entrypoint
//! HTML structure under the `zim://` CSP, surface routing, and the
//! configured smoke page actually exercising the engine API client.
//!
//! Delivery note (per the M5/M8 convention of recording documented
//! scope): §3.5 step 5's *execution* of the smoke page — a renderer
//! loading it and receiving the API's answer — belongs to the
//! browser integration (M13+), where a renderer exists. The engine
//! half shipped here statically verifies that the smoke page loads
//! the package's API client and targets the identity endpoint, so a
//! package cannot pass the gate without a runnable smoke page.

use crate::html;
use crate::package::FrontendPackage;
use crate::signature::TrustStore;

/// One named check outcome.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// The check's slug (stable API surface).
    pub name: &'static str,
    /// Whether the check passed.
    pub passed: bool,
    /// Human detail (empty when passing and uninteresting).
    pub detail: String,
}

/// The full gate report.
#[derive(Debug, Clone)]
pub struct SmokeReport {
    /// Every check, in execution order.
    pub checks: Vec<CheckResult>,
}

impl SmokeReport {
    /// Whether every check passed.
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// The first failure's detail (for error surfaces).
    pub fn first_failure(&self) -> Option<&CheckResult> {
        self.checks.iter().find(|c| !c.passed)
    }
}

impl std::fmt::Display for SmokeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "smoke test: {}",
            if self.passed() { "PASS" } else { "FAIL" }
        )?;
        for check in &self.checks {
            writeln!(
                f,
                "  [{}] {}{}",
                if check.passed { "ok" } else { "FAIL" },
                check.name,
                if check.detail.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", check.detail)
                }
            )?;
        }
        Ok(())
    }
}

/// Run the gate over one candidate package.
///
/// `engine_version` is the running engine's semver; `api_version`
/// the API generation string it advertises (e.g. `v1.0.0`).
pub fn run(
    package: &FrontendPackage,
    trust: &TrustStore,
    engine_version: &semver::Version,
    api_version: &str,
) -> SmokeReport {
    let mut checks = Vec::new();

    // 1. Manifest parsed + schema-validated (from_bytes already ran
    //    validate; re-assert so the report is self-contained).
    checks.push(CheckResult {
        name: "manifest",
        passed: package.manifest.validate().is_ok(),
        detail: if package.manifest.validate().is_ok() {
            format!(
                "{} {} ({})",
                package.manifest.id, package.manifest.version, package.manifest.name
            )
        } else {
            "manifest failed schema validation".to_owned()
        },
    });

    // 2. Engine compatibility.
    let requirement = package.manifest.zim_requirement();
    let compatible = requirement.matches(engine_version);
    checks.push(CheckResult {
        name: "engine_compatible",
        passed: compatible,
        detail: format!("engines.zim {} vs engine {}", requirement, engine_version),
    });

    // 3. API generation.
    let manifest_major = major_of(&package.manifest.api_version);
    let engine_major = major_of(api_version);
    let api_ok = manifest_major == engine_major && manifest_major != "0";
    checks.push(CheckResult {
        name: "api_version",
        passed: api_ok,
        detail: format!(
            "package {} vs engine {}",
            package.manifest.api_version, api_version
        ),
    });

    // 4. Signature classification.
    let signature = trust.check(&package.manifest);
    checks.push(CheckResult {
        name: "signature",
        passed: !signature.is_hard_reject(),
        detail: signature.as_str().to_owned(),
    });

    // 5. Entrypoints present.
    let mut missing: Vec<&String> = Vec::new();
    for path in package.manifest.entrypoints.values() {
        if package.file(path).is_none() {
            missing.push(path);
        }
    }
    checks.push(CheckResult {
        name: "entrypoints_present",
        passed: missing.is_empty(),
        detail: if missing.is_empty() {
            format!("{} entrypoint(s)", package.manifest.entrypoints.len())
        } else {
            format!(
                "missing: {}",
                missing
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    });

    // 6. Declared assets present.
    let mut missing_assets: Vec<&String> = Vec::new();
    for path in &package.manifest.assets {
        if package.file(path).is_none() {
            missing_assets.push(path);
        }
    }
    checks.push(CheckResult {
        name: "assets_present",
        passed: missing_assets.is_empty(),
        detail: if missing_assets.is_empty() {
            format!("{} asset(s)", package.manifest.assets.len())
        } else {
            format!(
                "missing: {}",
                missing_assets
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    });

    // 7. Every entrypoint is structurally sound under the CSP.
    let mut html_details = Vec::new();
    for (surface, path) in &package.manifest.entrypoints {
        let Some(bytes) = package.file(path) else {
            continue; // Reported by entrypoints_present.
        };
        if let Err(issues) = html::check_document(path, bytes) {
            html_details.push(format!(
                "{} ({}): {}",
                path,
                surface,
                issues
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    checks.push(CheckResult {
        name: "html_structure",
        passed: html_details.is_empty(),
        detail: html_details.join(" | "),
    });

    // 8. The smoke page exists and exercises the engine API.
    let smoke_detail = smoke_page_detail(package);
    checks.push(CheckResult {
        name: "smoke_page_exercises_api",
        passed: smoke_detail.is_empty(),
        detail: smoke_detail,
    });

    SmokeReport { checks }
}

/// Verify the configured smoke page (§3.5 step 5): it must exist,
/// reference the package's API client asset, and that client must
/// target the identity endpoint. Returns "" when satisfied.
fn smoke_page_detail(package: &FrontendPackage) -> String {
    let Some(smoke_path) = package.smoke_page() else {
        return "no smoke_test entry and no smoke.html/pages/smoke.html fallback".to_owned();
    };
    let Some(page) = package.file(&smoke_path).map(<[u8]>::to_vec) else {
        return format!("{smoke_path} is configured but missing from the package");
    };
    let page = String::from_utf8_lossy(&page);

    // The page must reference an api-client asset (the spec's
    // "loads zim-api.js" — the package's own API client module).
    let api_assets: Vec<&String> = package
        .manifest
        .assets
        .iter()
        .filter(|a| a.ends_with("api.js") || a.ends_with("zim-api.js"))
        .collect();
    if api_assets.is_empty() {
        return "the package declares no api.js client asset for its smoke page".to_owned();
    }
    let referenced = api_assets
        .iter()
        .any(|asset| page.contains(asset.as_str()) || page.contains("api.js"));
    if !referenced {
        return format!(
            "{smoke_path} does not reference any api client asset ({})",
            api_assets
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // The referenced client must call the identity endpoint (the
    // spec's `GET /api/v1/auth/whoami`; the engine's equivalent
    // surface is `GET /api/v1/users/me`).
    let calls_identity = api_assets.iter().any(|asset| {
        package
            .file(asset)
            .map(|bytes| {
                let text = String::from_utf8_lossy(bytes);
                text.contains("/users/me") || text.contains("/auth/whoami")
            })
            .unwrap_or(false)
    });
    if !calls_identity {
        return "the api client asset never calls /api/v1/users/me (or /auth/whoami)".to_owned();
    }
    String::new()
}

/// The major component of a version-ish string (`v1.0.0` → `v1`).
fn major_of(version: &str) -> String {
    let trimmed = version.trim();
    let base = trimmed.strip_prefix('v').unwrap_or(trimmed);
    format!("v{}", base.split('.').next().unwrap_or("0"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::FrontendManifest;
    use crate::package::FrontendPackage;
    use crate::signature::dev_sign_manifest;

    fn engine() -> (semver::Version, String) {
        (semver::Version::new(0, 0, 1), "v1.0.0".to_owned())
    }

    /// A minimal but complete custom package (pages + api client).
    fn custom_package() -> FrontendPackage {
        let mut manifest = FrontendManifest {
            id: "org.example.frontend.custom".to_owned(),
            manifest_version: 1,
            version: "0.1.0".to_owned(),
            name: "Custom".to_owned(),
            description: "A test package".to_owned(),
            author: "Example".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: std::collections::BTreeMap::from([("zim".to_owned(), ">=0.0.1".to_owned())]),
            full_replacement: false,
            smoke_test: Some("pages/smoke.html".to_owned()),
            page_surfaces: vec![
                "zim://newtab".to_owned(),
                "zim://servers/{id}/console".to_owned(),
            ],
            entrypoints: std::collections::BTreeMap::from([
                ("newtab".to_owned(), "pages/newtab.html".to_owned()),
                ("console".to_owned(), "pages/console.html".to_owned()),
            ]),
            assets: vec!["assets/api.js".to_owned()],
            permissions: vec!["websocket.connect".to_owned()],
            accessibility: None,
            signature: None,
        };
        dev_sign_manifest(&mut manifest);
        FrontendPackage::from_parts(
            manifest,
            vec![
                (
                    "pages/newtab.html".to_owned(),
                    br#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>x</title>
<script type="module" src="/zim/assets/api.js"></script>
</head><body><p>hi</p></body></html>"#
                        .to_vec(),
                ),
                (
                    "pages/console.html".to_owned(),
                    br#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>c</title>
<script type="module" src="/zim/assets/api.js"></script>
</head><body></body></html>"#
                        .to_vec(),
                ),
                (
                    "pages/smoke.html".to_owned(),
                    br#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>smoke</title>
<script type="module" src="/zim/assets/api.js"></script>
</head><body><p id="r">...</p></body></html>"#
                        .to_vec(),
                ),
                (
                    "assets/api.js".to_owned(),
                    b"export async function whoami(){ return fetch('/api/v1/users/me'); }".to_vec(),
                ),
            ],
        )
    }

    #[test]
    fn the_default_package_passes() {
        let (version, api) = engine();
        let report = run(
            &FrontendPackage::default_package(),
            &TrustStore::builtin(),
            &version,
            &api,
        );
        assert!(report.passed(), "{report}");
    }

    #[test]
    fn a_signed_custom_package_passes() {
        let (version, api) = engine();
        let report = run(&custom_package(), &TrustStore::builtin(), &version, &api);
        assert!(report.passed(), "{report}");
    }

    #[test]
    fn incompatible_engine_version_fails_the_gate() {
        let mut package = custom_package();
        package
            .manifest
            .engines
            .insert("zim".to_owned(), ">=99.0.0".to_owned());
        let (version, api) = engine();
        let report = run(&package, &TrustStore::builtin(), &version, &api);
        assert!(!report.passed());
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "engine_compatible" && !c.passed));
    }

    #[test]
    fn wrong_api_generation_fails_the_gate() {
        let mut package = custom_package();
        package.manifest.api_version = "v9".to_owned();
        let (version, api) = engine();
        let report = run(&package, &TrustStore::builtin(), &version, &api);
        assert!(!report.passed());
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "api_version" && !c.passed));
    }

    #[test]
    fn missing_entrypoint_fails_the_gate() {
        let package = custom_package();
        let files_without_console: Vec<(String, Vec<u8>)> = Vec::new();
        let stripped = FrontendPackage::from_parts(package.manifest.clone(), files_without_console);
        // Keep only the manifest: every entrypoint and asset missing.
        let (version, api) = engine();
        let report = run(&stripped, &TrustStore::builtin(), &version, &api);
        assert!(!report.passed());
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "entrypoints_present" && !c.passed));
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "assets_present" && !c.passed));
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "smoke_page_exercises_api" && !c.passed));
    }

    #[test]
    fn inline_script_fails_the_gate() {
        let package = custom_package();
        let mut files: Vec<(String, Vec<u8>)> = package
            .manifest
            .entrypoints
            .values()
            .map(|path| (path.clone(), package.file(path).unwrap().to_vec()))
            .collect();
        for (path, bytes) in &mut files {
            let mut text = String::from_utf8_lossy(bytes).to_string();
            if path.ends_with("newtab.html") {
                text = text.replace("</body>", "<script>bad()</script></body>");
            }
            *bytes = text.into_bytes();
        }
        files.push((
            "assets/api.js".to_owned(),
            b"fetch('/api/v1/users/me')".to_vec(),
        ));
        files.push((
            "pages/smoke.html".to_owned(),
            br#"<html><head></head><body><script src="/zim/assets/api.js"></script></body></html>"#
                .to_vec(),
        ));
        let tampered = FrontendPackage::from_parts(package.manifest.clone(), files);
        let (version, api) = engine();
        let report = run(&tampered, &TrustStore::builtin(), &version, &api);
        assert!(!report.passed());
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "html_structure" && !c.passed));
    }

    #[test]
    fn tampering_with_a_signed_manifest_fails_the_gate() {
        let mut package = custom_package();
        package.manifest.name = "Tampered After Signing".to_owned();
        let (version, api) = engine();
        let report = run(&package, &TrustStore::builtin(), &version, &api);
        assert!(!report.passed());
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "signature" && !c.passed));
        assert_eq!(
            report
                .checks
                .iter()
                .find(|c| c.name == "signature")
                .unwrap()
                .detail,
            "invalid"
        );
    }

    #[test]
    fn unsigned_packages_are_not_hard_rejects() {
        let mut package = custom_package();
        package.manifest.signature = None;
        let (version, api) = engine();
        let report = run(&package, &TrustStore::builtin(), &version, &api);
        // Unsigned needs approval, but does not fail the gate itself.
        assert!(report.passed(), "{report}");
        assert_eq!(
            report
                .checks
                .iter()
                .find(|c| c.name == "signature")
                .unwrap()
                .detail,
            "unsigned"
        );
    }

    #[test]
    fn a_smoke_page_without_an_api_call_fails_the_gate() {
        let package = custom_package();
        let files: Vec<(String, Vec<u8>)> = package
            .manifest
            .entrypoints
            .values()
            .map(|path| (path.clone(), package.file(path).unwrap().to_vec()))
            .chain([("assets/api.js".to_owned(), b"export const x = 1;".to_vec())])
            .collect();
        let stripped = FrontendPackage::from_parts(package.manifest.clone(), files);
        let (version, api) = engine();
        let report = run(&stripped, &TrustStore::builtin(), &version, &api);
        assert!(!report.passed());
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "smoke_page_exercises_api" && !c.passed));
    }
}
