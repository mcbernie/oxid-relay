//! Rhai engine setup and the curated host API exposed to plugin scripts.

use std::sync::Arc;

use rhai::{Dynamic, Engine, EvalAltResult, Map};

use crate::http::{HttpBody, HttpClient, HttpRequest};

/// Builds a Rhai engine with the host API registered.
///
/// Exposed functions:
/// - `http_get(url, headers) -> #{ status, body }`
/// - `http_post(url, headers, body) -> #{ status, body }`
/// - `http_post_form(url, headers, form) -> #{ status, body }`
/// - `to_json(value) -> string`
/// - `parse_json(string) -> value`
/// - `log_info(message)`
pub fn build_engine(http: Arc<dyn HttpClient>) -> Engine {
    let mut engine = Engine::new();
    // Allow moderately nested map/array literals (e.g. Graph sendMail payload).
    engine.set_max_expr_depths(256, 256);

    {
        let http = http.clone();
        engine.register_fn(
            "http_get",
            move |url: &str, headers: Map| -> Result<Map, Box<EvalAltResult>> {
                let request = HttpRequest {
                    method: "GET".to_string(),
                    url: url.to_string(),
                    headers: map_to_pairs(&headers)?,
                    body: HttpBody::None,
                };
                exec(http.as_ref(), request)
            },
        );
    }

    {
        let http = http.clone();
        engine.register_fn(
            "http_post",
            move |url: &str, headers: Map, body: &str| -> Result<Map, Box<EvalAltResult>> {
                let request = HttpRequest {
                    method: "POST".to_string(),
                    url: url.to_string(),
                    headers: map_to_pairs(&headers)?,
                    body: HttpBody::Text(body.to_string()),
                };
                exec(http.as_ref(), request)
            },
        );
    }

    {
        let http = http.clone();
        engine.register_fn(
            "http_post_form",
            move |url: &str, headers: Map, form: Map| -> Result<Map, Box<EvalAltResult>> {
                let request = HttpRequest {
                    method: "POST".to_string(),
                    url: url.to_string(),
                    headers: map_to_pairs(&headers)?,
                    body: HttpBody::Form(map_to_pairs(&form)?),
                };
                exec(http.as_ref(), request)
            },
        );
    }

    engine.register_fn(
        "to_json",
        |value: Dynamic| -> Result<String, Box<EvalAltResult>> {
            serde_json::to_string(&value)
                .map_err(|err| Box::<EvalAltResult>::from(format!("to_json: {err}")))
        },
    );

    engine.register_fn(
        "parse_json",
        |raw: &str| -> Result<Dynamic, Box<EvalAltResult>> {
            let value: serde_json::Value = serde_json::from_str(raw)
                .map_err(|err| Box::<EvalAltResult>::from(format!("parse_json: {err}")))?;
            rhai::serde::to_dynamic(value)
        },
    );

    engine.register_fn("log_info", |message: &str| {
        tracing::info!(target: "oxid_relay::plugin", "{message}");
    });

    engine
}

/// Executes a request through the client and maps the result into a Rhai map.
fn exec(http: &dyn HttpClient, request: HttpRequest) -> Result<Map, Box<EvalAltResult>> {
    let response = http
        .execute(request)
        .map_err(|err| Box::<EvalAltResult>::from(format!("http error: {err}")))?;

    let mut map = Map::new();
    map.insert("status".into(), (response.status as i64).into());
    map.insert("body".into(), response.body.into());
    Ok(map)
}

/// Converts a Rhai string map (headers/form) into ordered key/value pairs.
fn map_to_pairs(map: &Map) -> Result<Vec<(String, String)>, Box<EvalAltResult>> {
    let mut pairs = Vec::with_capacity(map.len());
    for (key, value) in map.iter() {
        let text = value.clone().into_string().map_err(|actual| {
            Box::<EvalAltResult>::from(format!("value for '{key}' must be a string, got {actual}"))
        })?;
        pairs.push((key.to_string(), text));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records requests and returns scripted responses by URL substring.
    struct MockHttp {
        rules: Vec<(String, u16, String)>,
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl HttpClient for MockHttp {
        fn execute(&self, request: HttpRequest) -> Result<HttpResponse, String> {
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

    use crate::http::HttpResponse;

    #[test]
    fn to_json_and_parse_json_roundtrip() {
        let engine = build_engine(Arc::new(MockHttp {
            rules: vec![],
            seen: Mutex::new(vec![]),
        }));
        let script = r#"
            let value = parse_json("{\"a\": 1, \"b\": \"x\"}");
            to_json(#{ a: value.a, b: value.b })
        "#;
        let result: String = engine.eval(script).expect("eval");
        assert!(result.contains("\"a\":1"));
        assert!(result.contains("\"b\":\"x\""));
    }

    #[test]
    fn http_post_records_request_and_returns_status() {
        let mock = Arc::new(MockHttp {
            rules: vec![("example.com".into(), 202, "ok".into())],
            seen: Mutex::new(vec![]),
        });
        let engine = build_engine(mock.clone());
        let script = r#"
            let res = http_post("https://example.com/x", #{ "Content-Type": "application/json" }, "{}");
            res.status
        "#;
        let status: i64 = engine.eval(script).expect("eval");
        assert_eq!(status, 202);

        let seen = mock.seen.lock().expect("lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].headers[0].0, "Content-Type");
    }
}
