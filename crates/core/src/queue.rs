//! Queue abstraction.
//!
//! The queue stores mails durably and hands them out for delivery. The
//! concrete backend (SQLite) lives in its own crate and implements [`Queue`].

use async_trait::async_trait;

use crate::error::Result;
use crate::message::{Mail, MailId, MailStatus};

/// Durable storage for mails awaiting delivery.
#[async_trait]
pub trait Queue: Send + Sync {
    /// Persists a new mail and returns it.
    async fn enqueue(&self, mail: Mail) -> Result<Mail>;

    /// Fetches up to `limit` mails that are ready to be delivered.
    async fn fetch_pending(&self, limit: u32) -> Result<Vec<Mail>>;

    /// Updates the delivery state of a mail.
    async fn update_status(
        &self,
        id: MailId,
        status: MailStatus,
        last_error: Option<String>,
    ) -> Result<()>;

    /// Returns a single mail by id.
    async fn get(&self, id: MailId) -> Result<Mail>;
}
