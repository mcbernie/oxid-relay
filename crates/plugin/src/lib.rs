//! Rhai based plugin system for OxidRelay.
//!
//! A plugin is a directory containing a `plugin.toml` manifest and a Rhai
//! script (default `script.rhai`). The manifest declares the plugin's name,
//! version, kind and capabilities; the script provides the behaviour, e.g. a
//! `send(mail, config)` function for a transport plugin.
//!
//! Scripts run in a Rhai engine that exposes a small, curated host API:
//! `http_get`, `http_post`, `http_post_form`, `to_json`, `parse_json` and
//! `log_info`. All HTTP access goes through the [`http::HttpClient`] trait, so
//! plugins can be tested against a mock without touching the network.

mod error;

pub mod engine;
pub mod http;
pub mod loader;
pub mod manifest;
pub mod transport;

pub use engine::build_engine;
pub use error::PluginError;
pub use http::{HttpBody, HttpClient, HttpRequest, HttpResponse, ReqwestClient};
pub use loader::{Plugin, discover, load_plugin, plugin_dirs};
pub use manifest::Manifest;
pub use transport::{RhaiTransport, string_config_map};
