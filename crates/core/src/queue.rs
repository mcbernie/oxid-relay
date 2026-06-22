//! Queue abstraction.
//!
//! The queue stores mails durably and hands them out for delivery. The
//! concrete backend (SQLite) lives in its own crate and implements [`Queue`].
//!
//! The lifecycle is modelled with explicit transitions so a dispatcher can
//! claim, complete, reschedule or bury a mail without ambiguous status writes.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::message::{Mail, MailId};

/// Durable storage for mails awaiting delivery.
#[async_trait]
pub trait Queue: Send + Sync {
    /// Persists a new mail and returns it.
    async fn enqueue(&self, mail: Mail) -> Result<Mail>;

    /// Fetches up to `limit` mails that are due for delivery at `now`
    /// (status pending or failed and `next_attempt_at <= now`), oldest first.
    async fn fetch_due(&self, limit: u32, now: DateTime<Utc>) -> Result<Vec<Mail>>;

    /// Marks a mail as in-flight so it is not picked up again. Does not count
    /// as a delivery attempt.
    async fn mark_sending(&self, id: MailId) -> Result<()>;

    /// Marks a mail as successfully delivered.
    async fn mark_sent(&self, id: MailId) -> Result<()>;

    /// Records a failed attempt and reschedules the mail for `retry_at`.
    /// Increments the attempt counter.
    async fn mark_failed(&self, id: MailId, error: String, retry_at: DateTime<Utc>) -> Result<()>;

    /// Records a permanently failed mail. Increments the attempt counter.
    async fn mark_dead(&self, id: MailId, error: String) -> Result<()>;

    /// Resets in-flight (sending) mails back to pending. Used at startup to
    /// recover mails left in-flight by a crash. Returns the number reset.
    async fn requeue_sending(&self) -> Result<u64>;

    /// Returns a single mail by id.
    async fn get(&self, id: MailId) -> Result<Mail>;
}
