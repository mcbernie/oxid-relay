//! HTTP access for plugins.
//!
//! Scripts never touch the network directly. They call host functions that go
//! through the [`HttpClient`] trait, so a mock client can be injected in tests.

/// Request body variants a plugin can send.
#[derive(Debug, Clone)]
pub enum HttpBody {
    /// No body.
    None,
    /// A raw text body (e.g. JSON).
    Text(String),
    /// A form-urlencoded body.
    Form(Vec<(String, String)>),
}

/// An outgoing HTTP request built by a plugin.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// HTTP method, e.g. `"GET"` or `"POST"`.
    pub method: String,
    /// Target URL.
    pub url: String,
    /// Request headers as key/value pairs.
    pub headers: Vec<(String, String)>,
    /// Request body.
    pub body: HttpBody,
}

/// The response returned to a plugin.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body as text.
    pub body: String,
}

/// Abstraction over the HTTP layer used by plugins.
pub trait HttpClient: Send + Sync {
    /// Executes the request and returns the response, or an error string.
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, String>;
}

/// Default [`HttpClient`] backed by a blocking `reqwest` client.
///
/// The blocking client is built per request so that its internal runtime is
/// created and dropped on the calling (blocking) thread. Requests only run
/// inside `spawn_blocking`, never on an async runtime thread.
#[derive(Default)]
pub struct ReqwestClient;

impl ReqwestClient {
    /// Creates a new client handle. Holds no resources itself.
    pub fn new() -> Result<Self, String> {
        Ok(Self)
    }
}

impl HttpClient for ReqwestClient {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|err| err.to_string())?;

        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|err| format!("invalid HTTP method: {err}"))?;

        let mut builder = client.request(method, &request.url);
        for (key, value) in &request.headers {
            builder = builder.header(key, value);
        }
        builder = match request.body {
            HttpBody::None => builder,
            HttpBody::Text(text) => builder.body(text),
            HttpBody::Form(form) => builder.form(&form),
        };

        let response = builder.send().map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let body = response.text().map_err(|err| err.to_string())?;
        Ok(HttpResponse { status, body })
    }
}
