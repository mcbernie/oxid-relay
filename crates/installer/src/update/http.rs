//! Minimal HTTP abstraction so the release lookup and download flow can be
//! tested without network access.

use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;

/// Base URL of the GitHub REST API.
pub const GITHUB_API: &str = "https://api.github.com";

/// Why an HTTP request failed.
#[derive(Debug)]
pub enum HttpError {
    /// The server answered with a non-success status code.
    Status {
        /// Requested URL.
        url: String,
        /// HTTP status code.
        code: u16,
        /// Whether the response indicates an exhausted rate limit.
        rate_limited: bool,
    },
    /// The request could not be sent or the body could not be read.
    Transport {
        /// Requested URL.
        url: String,
        /// Underlying error, rendered as text.
        message: String,
    },
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status { url, code, .. } => write!(f, "GET {url} returned HTTP {code}"),
            Self::Transport { url, message } => write!(f, "GET {url} failed: {message}"),
        }
    }
}

impl std::error::Error for HttpError {}

/// Fetches the body of a URL with a GET request.
pub trait HttpClient {
    /// Returns the full response body, or an error for transport failures and
    /// non-success status codes (redirects are followed).
    fn get(&self, url: &str) -> Result<Vec<u8>, HttpError>;
}

/// Fetches `url` and deserializes the body as JSON.
pub fn get_json<T: DeserializeOwned>(http: &dyn HttpClient, url: &str) -> anyhow::Result<T> {
    let body = http.get(url)?;
    serde_json::from_slice(&body).map_err(|e| anyhow::anyhow!("invalid JSON from {url}: {e}"))
}

/// [`HttpClient`] backed by a blocking `reqwest` client.
pub struct ReqwestClient {
    client: reqwest::blocking::Client,
}

impl ReqwestClient {
    /// Connect timeout for every request.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
    /// Overall timeout per request, generous enough for release archives.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

    /// Builds a client identifying itself as `oxid-relay-updater/<VERSION>`.
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(format!("oxid-relay-updater/{}", crate::VERSION))
            .connect_timeout(Self::CONNECT_TIMEOUT)
            .timeout(Self::REQUEST_TIMEOUT)
            .build()?;
        Ok(Self { client })
    }
}

impl HttpClient for ReqwestClient {
    fn get(&self, url: &str) -> Result<Vec<u8>, HttpError> {
        let transport = |e: reqwest::Error| HttpError::Transport {
            url: url.to_owned(),
            message: error_chain(&e),
        };
        // The API wants its own media type; release downloads are plain files.
        let accept = if url.starts_with(GITHUB_API) {
            "application/vnd.github+json"
        } else {
            "application/octet-stream"
        };
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, accept)
            .send()
            .map_err(transport)?;
        let status = response.status();
        if !status.is_success() {
            let remaining = response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok());
            return Err(HttpError::Status {
                url: url.to_owned(),
                code: status.as_u16(),
                rate_limited: status.as_u16() == 429
                    || (status.as_u16() == 403 && remaining == Some("0")),
            });
        }
        response.bytes().map(|b| b.to_vec()).map_err(transport)
    }
}

/// Formats an error with all its sources ("a: b: c"), since reqwest's own
/// message ("error sending request") hides the actual cause (DNS, TLS, ...).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// In-memory [`HttpClient`] for tests: serves fixed bodies per URL and
/// answers 404 for everything else.
#[cfg(test)]
#[derive(Default)]
pub struct FakeHttp {
    responses: std::collections::HashMap<String, Result<Vec<u8>, u16>>,
}

#[cfg(test)]
impl FakeHttp {
    /// Serves `body` for `url`.
    pub fn with(mut self, url: &str, body: impl Into<Vec<u8>>) -> Self {
        self.responses.insert(url.to_owned(), Ok(body.into()));
        self
    }

    /// Answers `url` with HTTP status `code`.
    pub fn with_status(mut self, url: &str, code: u16) -> Self {
        self.responses.insert(url.to_owned(), Err(code));
        self
    }
}

#[cfg(test)]
impl HttpClient for FakeHttp {
    fn get(&self, url: &str) -> Result<Vec<u8>, HttpError> {
        match self.responses.get(url) {
            Some(Ok(body)) => Ok(body.clone()),
            other => {
                let code = other.and_then(|r| r.as_ref().err().copied()).unwrap_or(404);
                Err(HttpError::Status {
                    url: url.to_owned(),
                    code,
                    rate_limited: code == 429,
                })
            }
        }
    }
}
