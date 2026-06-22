//! Adapts a loaded Rhai plugin to the core [`Transport`] trait.

use std::sync::Arc;

use async_trait::async_trait;
use oxid_relay_core::{CoreError, Mail, Result, Transport};
use rhai::{AST, Dynamic, Engine, Map, Scope};

use crate::loader::Plugin;
use crate::manifest::Manifest;

/// A transport whose behaviour comes from a Rhai plugin script.
///
/// The script must define `fn send(mail, config)`. Delivery runs on a blocking
/// thread because the Rhai engine and the HTTP host functions are synchronous.
pub struct RhaiTransport {
    manifest: Manifest,
    engine: Arc<Engine>,
    ast: Arc<AST>,
    config: Map,
}

impl RhaiTransport {
    /// Wraps a loaded plugin together with the shared engine and a per-plugin
    /// configuration map passed to the script as the `config` argument.
    pub fn new(plugin: Plugin, engine: Arc<Engine>, config: Map) -> Self {
        Self {
            manifest: plugin.manifest,
            engine,
            ast: Arc::new(plugin.ast),
            config,
        }
    }

    /// Builds the `mail` map handed to the script.
    fn mail_to_map(mail: &Mail) -> Map {
        let mut from = Map::new();
        from.insert("email".into(), mail.from.email.clone().into());
        if let Some(name) = &mail.from.name {
            from.insert("name".into(), name.clone().into());
        }

        let mut recipients = rhai::Array::new();
        for addr in &mail.to {
            let mut entry = Map::new();
            entry.insert("email".into(), addr.email.clone().into());
            if let Some(name) = &addr.name {
                entry.insert("name".into(), name.clone().into());
            }
            recipients.push(Dynamic::from_map(entry));
        }

        let mut map = Map::new();
        map.insert("from".into(), Dynamic::from_map(from));
        map.insert("to".into(), Dynamic::from_array(recipients));
        map.insert("subject".into(), mail.subject.clone().into());
        map.insert("body".into(), mail.body.clone().into());
        map
    }
}

#[async_trait]
impl Transport for RhaiTransport {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    async fn send(&self, mail: &Mail) -> Result<()> {
        let engine = self.engine.clone();
        let ast = self.ast.clone();
        let config = self.config.clone();
        let mail_map = Self::mail_to_map(mail);
        let mail_id = mail.id;

        let outcome = tokio::task::spawn_blocking(move || {
            let mut scope = Scope::new();
            engine.call_fn::<()>(&mut scope, &ast, "send", (mail_map, config))
        })
        .await
        .map_err(|err| CoreError::Transport(format!("plugin task failed: {err}")))?;

        outcome.map_err(|err| CoreError::Transport(format!("plugin send: {err}")))?;
        tracing::info!(mail_id = %mail_id, plugin = %self.manifest.name, "mail delivered via plugin");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_engine;
    use crate::http::{HttpClient, HttpRequest, HttpResponse};
    use crate::loader::load_plugin;
    use oxid_relay_core::{Address, NewMail};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use uuid::Uuid;

    /// Mock that scripts responses by URL substring and records every request.
    struct MockHttp {
        rules: Vec<(String, u16, String)>,
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl HttpClient for MockHttp {
        fn execute(
            &self,
            request: HttpRequest,
        ) -> std::result::Result<HttpResponse, String> {
            self.seen.lock().expect("lock").push(request.clone());
            for (needle, status, body) in &self.rules {
                if request.url.contains(needle) {
                    return Ok(HttpResponse {
                        status: *status,
                        body: body.clone(),
                    });
                }
            }
            Err(format!("no mock rule for {}", request.url))
        }
    }

    fn graph_plugin_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/graph")
            .canonicalize()
            .expect("graph plugin dir exists")
    }

    fn sample_mail() -> Mail {
        Mail::from_new(
            NewMail {
                from: Address::new("relay@example.com"),
                to: vec![Address::new("ziel@example.com")],
                subject: "Status".into(),
                body: "Okay".into(),
                transport: Some("graph".into()),
            },
            chrono::Utc::now(),
            Uuid::new_v4(),
        )
    }

    fn graph_config() -> Map {
        let mut config = Map::new();
        config.insert("tenant_id".into(), "tenant".into());
        config.insert("client_id".into(), "client".into());
        config.insert("client_secret".into(), "secret".into());
        config.insert("sender".into(), "relay@example.com".into());
        config
    }

    #[tokio::test]
    async fn graph_plugin_sends_via_token_then_sendmail() {
        let mock = Arc::new(MockHttp {
            rules: vec![
                (
                    "login.microsoftonline.com".into(),
                    200,
                    r#"{"access_token":"abc123"}"#.into(),
                ),
                ("graph.microsoft.com".into(), 202, String::new()),
            ],
            seen: Mutex::new(vec![]),
        });

        let engine = build_engine(mock.clone());
        let plugin = load_plugin(&engine, &graph_plugin_dir()).expect("load graph plugin");
        assert_eq!(plugin.manifest.name, "graph");

        let transport = RhaiTransport::new(plugin, Arc::new(engine), graph_config());
        transport.send(&sample_mail()).await.expect("send");

        let seen = mock.seen.lock().expect("lock");
        assert_eq!(seen.len(), 2, "token request then sendMail");
        assert!(seen[0].url.contains("login.microsoftonline.com"));
        assert!(seen[1].url.contains("/sendMail"));
        // The second request must carry the bearer token from the first.
        assert!(
            seen[1]
                .headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer abc123")
        );
    }

    #[tokio::test]
    async fn graph_plugin_propagates_token_failure() {
        let mock = Arc::new(MockHttp {
            rules: vec![(
                "login.microsoftonline.com".into(),
                401,
                "denied".into(),
            )],
            seen: Mutex::new(vec![]),
        });

        let engine = build_engine(mock.clone());
        let plugin = load_plugin(&engine, &graph_plugin_dir()).expect("load graph plugin");
        let transport = RhaiTransport::new(plugin, Arc::new(engine), graph_config());

        let err = transport.send(&sample_mail()).await.expect_err("must fail");
        assert!(matches!(err, CoreError::Transport(_)));
    }
}
