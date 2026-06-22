//! Plugin manifest (`plugin.toml`).

use serde::Deserialize;

/// Metadata a plugin declares about itself.
///
/// Read from the `plugin.toml` file in the plugin directory.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Stable plugin name, also used as the transport name.
    pub name: String,
    /// Plugin version string.
    pub version: String,
    /// Plugin kind, e.g. `"transport"`.
    pub kind: String,
    /// Declared capabilities, e.g. `["send"]`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Human readable description.
    #[serde(default)]
    pub description: String,
    /// Script file name relative to the plugin directory.
    #[serde(default = "default_entry")]
    pub entry: String,
}

fn default_entry() -> String {
    "script.rhai".to_string()
}

impl Manifest {
    /// Returns whether the plugin declares the given capability.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest_with_defaults() {
        let raw = r#"
            name = "graph"
            version = "0.1.0"
            kind = "transport"
            capabilities = ["send"]
            description = "Graph transport"
        "#;
        let manifest: Manifest = toml::from_str(raw).expect("valid manifest");
        assert_eq!(manifest.name, "graph");
        assert_eq!(manifest.entry, "script.rhai");
        assert!(manifest.has_capability("send"));
        assert!(!manifest.has_capability("receive"));
    }

    #[test]
    fn rejects_unknown_field() {
        let raw = r#"
            name = "x"
            version = "1"
            kind = "transport"
            unexpected = true
        "#;
        assert!(toml::from_str::<Manifest>(raw).is_err());
    }
}
