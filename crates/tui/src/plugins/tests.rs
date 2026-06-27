use crate::plugins::manifest::{load_manifest, load_plugin_mcp, PluginManifest};

#[test]
fn manifest_parsing_basics() {
    let toml_str = r#"
[plugin]
name = "test-plugin"
description = "A test plugin"
"#;
    let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
    assert_eq!(manifest.plugin.name, "test-plugin");
    assert_eq!(manifest.plugin.description, "A test plugin");
}

#[test]
fn load_plugin_mcp_empty_when_none() {
    let toml_str = r#"
[plugin]
name = "no-mcp"
description = "no mcp"
"#;
    let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
    let mcp = load_plugin_mcp(&manifest);
    assert!(mcp.is_empty());
}

#[test]
fn load_manifest_fails_for_missing_dir() {
    let dir = std::path::Path::new("/nonexistent/plugin/dir");
    let result = load_manifest(dir);
    assert!(result.is_err());
}
