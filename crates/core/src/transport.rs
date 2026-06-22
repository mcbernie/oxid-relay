//! Transport abstraction.
//!
//! Concrete transports (SMTP, Microsoft Graph, SES, ...) live in their own
//! crates and implement the [`Transport`] trait. The core never depends on a
//! concrete implementation.

use async_trait::async_trait;

use crate::error::Result;
use crate::message::Mail;

/// Stable name identifying a transport, e.g. `"smtp"` or `"graph"`.
pub type TransportName = String;

/// A channel capable of delivering a [`Mail`].
#[async_trait]
pub trait Transport: Send + Sync {
    /// Returns the stable name of this transport.
    fn name(&self) -> &str;

    /// Delivers a single mail. Returns an error if delivery failed; the caller
    /// decides whether to retry based on the queue policy.
    async fn send(&self, mail: &Mail) -> Result<()>;
}
