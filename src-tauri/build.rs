fn main() {
    let mcp_cap_path = std::path::Path::new("capabilities/mcp-bridge.json");
    #[cfg(all(feature = "mcp-bridge", debug_assertions))]
    {
        let cap = r#"{
  "identifier": "mcp-bridge",
  "description": "enables MCP bridge for development",
  "windows": ["main"],
  "permissions": ["mcp-bridge:default"]
}"#;
        std::fs::write(mcp_cap_path, cap)
            .expect("failed to write mcp-bridge capability");
    }
    #[cfg(not(all(feature = "mcp-bridge", debug_assertions)))]
    {
        let _ = std::fs::remove_file(mcp_cap_path);
    }

    // tauri-build embeds its Windows manifest (the Common Controls v6 dependency) through
    // embed-resource, which links it into BINS only. The lib's unit-test exe then loads
    // comctl32 v5, which has no TaskDialogIndirect, and Windows refuses to start it:
    // STATUS_ENTRYPOINT_NOT_FOUND before a single test runs. So take the manifest away from
    // tauri-build and hand the same file to the linker for every target — app and tests alike.
    let windows = if embed_windows_manifest() {
        tauri_build::WindowsAttributes::new_without_app_manifest()
    } else {
        tauri_build::WindowsAttributes::new()
    };
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to run tauri-build")
}

/// Embeds `windows-app-manifest.xml` via the MSVC linker. Returns false on any other target,
/// where tauri-build's own handling is left alone.
fn embed_windows_manifest() -> bool {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return false;
    }
    let manifest = std::env::current_dir().unwrap().join("windows-app-manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    true
}
