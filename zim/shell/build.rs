//! Build script for the ZIM shell: Tauri's resource/context
//! embedding (icons, bundled `ui/` pages, capability schemas), plus
//! the Windows application-manifest strategy (M25, CI batch 10).

use std::{env, fs, path::PathBuf};

/// The application manifest every Windows executable built from
/// this crate embeds.
///
/// The Common-Controls v6 dependency is load-bearing: the Tauri
/// windowing stack statically imports `TaskDialogIndirect`, which
/// exists ONLY in the v6 side-by-side comctl32 assembly. An
/// executable without this manifest resolves `comctl32.dll` to the
/// 5.82 legacy DLL and dies at process start with
/// `STATUS_ENTRYPOINT_NOT_FOUND` (0xc0000139) — exactly what the
/// Windows CI leg was showing on `cargo test --lib` before the
/// lib's unittest executable grew a manifest.
const WINDOWS_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    // Tauri's resource embedding — icons, version info, sidecars,
    // capability schemas. The manifest is deliberately LEFT OUT of
    // the generated .res (`new_without_app_manifest`): tauri-winres
    // hands the .res to embed-resource, whose `compile()` links it
    // with `cargo:rustc-link-arg-bins` — bins ONLY. Cargo's
    // link-arg matching (`LinkArgTarget::applies_to`) treats a
    // lib-as-test unit's target as the lib, so neither the `-bins`
    // nor the `-tests` flavor ever reaches the lib's unittest
    // executable — it was shipped manifest-less and died at load.
    //
    // Instead the manifest is embedded below via plain
    // `cargo:rustc-link-arg` (the `All` flavor), which cargo
    // applies to every artifact it links from this crate: the bin,
    // the bin's own unittest exe, the lib's unittest exe, and the
    // integration-test executables. One source of truth, and no
    // duplicate RT_MANIFEST resources on any image.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("failed to run tauri-build");

    // Build scripts compile for the HOST, so gate on the TARGET
    // cargo reports (CARGO_CFG_*), never on cfg!(windows). The
    // /MANIFEST* switches are MSVC link.exe syntax; the windows-gnu
    // toolchain's ld does not understand them.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
        let manifest = out_dir.join("zim-shell.exe.manifest");
        fs::write(&manifest, WINDOWS_MANIFEST).expect("write the Windows application manifest");
        // Plain `cargo:rustc-link-arg` (the All flavor): every
        // executable linked from this crate embeds the manifest.
        // Multiple link args are each forwarded as their own
        // link.exe argument.
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }

    // The manifest content lives in this file's source, not on
    // disk — rerun when the build script itself changes.
    println!("cargo:rerun-if-changed=build.rs");
}
