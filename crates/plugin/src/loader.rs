//! Plugin discovery and loading.

use std::path::{Path, PathBuf};

use rhai::{AST, Engine};

use crate::error::PluginError;
use crate::manifest::Manifest;

/// A loaded plugin: its manifest, compiled script and source directory.
pub struct Plugin {
    /// Declared metadata.
    pub manifest: Manifest,
    /// Compiled Rhai script.
    pub ast: AST,
    /// Directory the plugin was loaded from.
    pub dir: PathBuf,
}

/// Returns the directories scanned for plugins.
///
/// Debug builds use a local `plugins` directory for development. Release builds
/// use the platform-typical system location.
pub fn plugin_dirs() -> Vec<PathBuf> {
    if cfg!(debug_assertions) {
        return vec![PathBuf::from("plugins")];
    }

    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".to_string());
        vec![PathBuf::from(base).join("OxidRelay").join("plugins")]
    }
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from("/Library/Application Support/OxidRelay/plugins")]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![PathBuf::from("/etc/oxid-relay/plugins")]
    }
}

/// Loads a single plugin from its directory (containing `plugin.toml`).
pub fn load_plugin(engine: &Engine, dir: &Path) -> Result<Plugin, PluginError> {
    let manifest_path = dir.join("plugin.toml");
    let raw = std::fs::read_to_string(&manifest_path).map_err(|source| PluginError::Io {
        path: manifest_path.display().to_string(),
        source,
    })?;
    let manifest: Manifest = toml::from_str(&raw)?;

    let script_path = dir.join(&manifest.entry);
    let script = std::fs::read_to_string(&script_path).map_err(|source| PluginError::Io {
        path: script_path.display().to_string(),
        source,
    })?;

    let ast = engine
        .compile(&script)
        .map_err(|err| PluginError::Compile(err.to_string()))?;

    Ok(Plugin {
        manifest,
        ast,
        dir: dir.to_path_buf(),
    })
}

/// Discovers and loads all plugins in the given directory.
///
/// A plugin is any sub-directory containing a `plugin.toml`. A missing
/// directory yields an empty list rather than an error.
pub fn discover(engine: &Engine, dir: &Path) -> Result<Vec<Plugin>, PluginError> {
    let mut plugins = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(plugins),
    };

    for entry in entries {
        let entry = entry.map_err(|source| PluginError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() || !plugin_dir.join("plugin.toml").exists() {
            continue;
        }
        plugins.push(load_plugin(engine, &plugin_dir)?);
    }

    Ok(plugins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_engine;
    use crate::http::{HttpClient, HttpRequest, HttpResponse};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

    struct NoopHttp;
    impl HttpClient for NoopHttp {
        fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, String> {
            Err("not used".to_string())
        }
    }

    /// Creates a unique temp directory holding one plugin.
    fn temp_plugin(manifest: &str, script: &str) -> PathBuf {
        let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("oxidrelay_plugins_{pid}_{n}"));
        let plugin = root.join("sample");
        std::fs::create_dir_all(&plugin).expect("create dirs");
        std::fs::write(plugin.join("plugin.toml"), manifest).expect("write manifest");
        std::fs::write(plugin.join("script.rhai"), script).expect("write script");
        root
    }

    #[test]
    fn discovers_plugin_in_directory() {
        let root = temp_plugin(
            r#"
                name = "sample"
                version = "0.1.0"
                kind = "transport"
                capabilities = ["send"]
            "#,
            "fn send(mail, config) { }",
        );
        let engine = build_engine(Arc::new(NoopHttp));
        let plugins = discover(&engine, &root).expect("discover");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "sample");
    }

    #[test]
    fn missing_directory_yields_empty() {
        let engine = build_engine(Arc::new(NoopHttp));
        let plugins = discover(&engine, Path::new("/does/not/exist/oxidrelay")).expect("discover");
        assert!(plugins.is_empty());
    }

    #[test]
    fn compile_error_is_reported() {
        let root = temp_plugin(
            r#"
                name = "broken"
                version = "0.1.0"
                kind = "transport"
            "#,
            "fn send(mail, config) { this is not valid rhai",
        );
        let engine = build_engine(Arc::new(NoopHttp));
        let plugin_dir = root.join("sample");
        assert!(matches!(
            load_plugin(&engine, &plugin_dir),
            Err(PluginError::Compile(_))
        ));
    }
}
