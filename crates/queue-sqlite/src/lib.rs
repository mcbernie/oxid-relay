//! SQLite backed implementation of the core [`Queue`] trait.
//!
//! Recipients and the sender address are stored as JSON so the schema stays
//! flat while the core domain model keeps its rich types.

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use oxid_relay_core::{Address, CoreError, Mail, MailId, MailStatus, Queue, Result};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use uuid::Uuid;

/// Maps a [`MailStatus`] to its stored string representation.
fn status_to_str(status: MailStatus) -> &'static str {
    match status {
        MailStatus::Pending => "pending",
        MailStatus::Sending => "sending",
        MailStatus::Sent => "sent",
        MailStatus::Failed => "failed",
        MailStatus::Dead => "dead",
    }
}

/// Parses a stored status string back into a [`MailStatus`].
fn status_from_str(raw: &str) -> Result<MailStatus> {
    match raw {
        "pending" => Ok(MailStatus::Pending),
        "sending" => Ok(MailStatus::Sending),
        "sent" => Ok(MailStatus::Sent),
        "failed" => Ok(MailStatus::Failed),
        "dead" => Ok(MailStatus::Dead),
        other => Err(CoreError::Queue(format!("unknown status: {other}"))),
    }
}

/// Parses an RFC 3339 timestamp from storage.
fn parse_time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| CoreError::Queue(format!("decode timestamp: {err}")))
}

/// SQLite queue backend.
pub struct SqliteQueue {
    pool: SqlitePool,
}

impl SqliteQueue {
    /// Connects to the given SQLite URL (e.g. `sqlite://queue.db`), creating
    /// the database file if necessary, and ensures the schema exists.
    pub async fn connect(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|err| CoreError::Queue(err.to_string()))?
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|err| CoreError::Queue(err.to_string()))?;

        let queue = Self { pool };
        queue.migrate().await?;
        Ok(queue)
    }

    /// Creates the `mails` table if it does not exist yet.
    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS mails (
                id          TEXT PRIMARY KEY NOT NULL,
                sender      TEXT NOT NULL,
                recipients  TEXT NOT NULL,
                subject     TEXT NOT NULL,
                body        TEXT NOT NULL,
                transport   TEXT,
                status      TEXT NOT NULL,
                attempts    INTEGER NOT NULL DEFAULT 0,
                last_error  TEXT,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                next_attempt_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;
        Ok(())
    }

    /// Reconstructs a [`Mail`] from a database row.
    fn row_to_mail(row: &SqliteRow) -> Result<Mail> {
        let id: String = row.get("id");
        let sender: String = row.get("sender");
        let recipients: String = row.get("recipients");
        let status: String = row.get("status");
        let attempts: i64 = row.get("attempts");
        let created_at: String = row.get("created_at");
        let updated_at: String = row.get("updated_at");
        let next_attempt_at: String = row.get("next_attempt_at");

        let from: Address = serde_json::from_str(&sender)
            .map_err(|err| CoreError::Queue(format!("decode sender: {err}")))?;
        let to: Vec<Address> = serde_json::from_str(&recipients)
            .map_err(|err| CoreError::Queue(format!("decode recipients: {err}")))?;

        Ok(Mail {
            id: Uuid::parse_str(&id)
                .map_err(|err| CoreError::Queue(format!("decode id: {err}")))?,
            from,
            to,
            subject: row.get("subject"),
            body: row.get("body"),
            transport: row.get("transport"),
            status: status_from_str(&status)?,
            attempts: attempts as u32,
            last_error: row.get("last_error"),
            created_at: parse_time(&created_at)?,
            updated_at: parse_time(&updated_at)?,
            next_attempt_at: parse_time(&next_attempt_at)?,
        })
    }

    /// Applies a status transition in a single statement.
    ///
    /// `last_error` is always written (NULL clears it). `bump_attempts` adds one
    /// to the attempt counter. `next_attempt_at`, when set, reschedules the mail;
    /// otherwise the existing value is kept.
    async fn set_status(
        &self,
        id: MailId,
        status: MailStatus,
        last_error: Option<String>,
        bump_attempts: bool,
        next_attempt_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE mails
            SET status = ?,
                last_error = ?,
                updated_at = ?,
                attempts = attempts + ?,
                next_attempt_at = COALESCE(?, next_attempt_at)
            WHERE id = ?
            "#,
        )
        .bind(status_to_str(status))
        .bind(last_error)
        .bind(Utc::now().to_rfc3339())
        .bind(if bump_attempts { 1_i64 } else { 0_i64 })
        .bind(next_attempt_at.map(|t| t.to_rfc3339()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound(id.to_string()));
        }
        Ok(())
    }
}

#[async_trait]
impl Queue for SqliteQueue {
    async fn enqueue(&self, mail: Mail) -> Result<Mail> {
        let sender =
            serde_json::to_string(&mail.from).map_err(|err| CoreError::Queue(err.to_string()))?;
        let recipients =
            serde_json::to_string(&mail.to).map_err(|err| CoreError::Queue(err.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO mails
                (id, sender, recipients, subject, body, transport,
                 status, attempts, last_error, created_at, updated_at, next_attempt_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(mail.id.to_string())
        .bind(sender)
        .bind(recipients)
        .bind(&mail.subject)
        .bind(&mail.body)
        .bind(&mail.transport)
        .bind(status_to_str(mail.status))
        .bind(mail.attempts as i64)
        .bind(&mail.last_error)
        .bind(mail.created_at.to_rfc3339())
        .bind(mail.updated_at.to_rfc3339())
        .bind(mail.next_attempt_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        Ok(mail)
    }

    async fn fetch_due(&self, limit: u32, now: DateTime<Utc>) -> Result<Vec<Mail>> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM mails
            WHERE status IN ('pending', 'failed')
              AND next_attempt_at <= ?
            ORDER BY next_attempt_at ASC
            LIMIT ?
            "#,
        )
        .bind(now.to_rfc3339())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        rows.iter().map(Self::row_to_mail).collect()
    }

    async fn mark_sending(&self, id: MailId) -> Result<()> {
        self.set_status(id, MailStatus::Sending, None, false, None)
            .await
    }

    async fn mark_sent(&self, id: MailId) -> Result<()> {
        self.set_status(id, MailStatus::Sent, None, false, None).await
    }

    async fn mark_failed(&self, id: MailId, error: String, retry_at: DateTime<Utc>) -> Result<()> {
        self.set_status(id, MailStatus::Failed, Some(error), true, Some(retry_at))
            .await
    }

    async fn mark_dead(&self, id: MailId, error: String) -> Result<()> {
        self.set_status(id, MailStatus::Dead, Some(error), true, None)
            .await
    }

    async fn requeue_sending(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE mails
            SET status = 'pending', updated_at = ?
            WHERE status = 'sending'
            "#,
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        Ok(result.rows_affected())
    }

    async fn requeue_stale_sending(&self, older_than: DateTime<Utc>) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE mails
            SET status = 'pending', updated_at = ?
            WHERE status = 'sending' AND updated_at < ?
            "#,
        )
        .bind(Utc::now().to_rfc3339())
        .bind(older_than.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        Ok(result.rows_affected())
    }

    async fn get(&self, id: MailId) -> Result<Mail> {
        let row = sqlx::query("SELECT * FROM mails WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| CoreError::Queue(err.to_string()))?;

        match row {
            Some(row) => Self::row_to_mail(&row),
            None => Err(CoreError::NotFound(id.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxid_relay_core::NewMail;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DB_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Builds a fresh, isolated in-memory queue per test.
    ///
    /// A unique shared-cache name keeps every pool connection on the same
    /// in-memory database while avoiding collisions between tests.
    async fn memory_queue() -> SqliteQueue {
        let n = DB_COUNTER.fetch_add(1, Ordering::SeqCst);
        let url = format!("sqlite:file:oxidrelay_test_{n}?mode=memory&cache=shared");
        SqliteQueue::connect(&url).await.expect("in-memory queue")
    }

    fn sample() -> Mail {
        Mail::from_new(
            NewMail {
                from: Address::new("relay@example.com"),
                to: vec![Address {
                    email: "ziel@example.com".into(),
                    name: Some("Zustellung".into()),
                }],
                subject: "Grüße".into(),
                body: "Körper".into(),
                transport: None,
            },
            Utc::now(),
            Uuid::new_v4(),
        )
    }

    #[tokio::test]
    async fn enqueue_and_get_roundtrip() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        let loaded = queue.get(id).await.expect("get");
        assert_eq!(loaded.subject, "Grüße");
        assert_eq!(loaded.status, MailStatus::Pending);
        assert_eq!(loaded.to.len(), 1);
        assert_eq!(loaded.to[0].name.as_deref(), Some("Zustellung"));
    }

    #[tokio::test]
    async fn get_unknown_returns_not_found() {
        let queue = memory_queue().await;
        let err = queue.get(Uuid::new_v4()).await.expect_err("not found");
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn fetch_due_returns_pending() {
        let queue = memory_queue().await;
        queue.enqueue(sample()).await.expect("enqueue");

        let due = queue.fetch_due(10, Utc::now()).await.expect("fetch");
        assert_eq!(due.len(), 1);
    }

    #[tokio::test]
    async fn fetch_due_skips_sent() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue.mark_sent(id).await.expect("mark sent");

        let due = queue.fetch_due(10, Utc::now()).await.expect("fetch");
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn fetch_due_skips_in_flight() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue.mark_sending(id).await.expect("mark sending");

        let due = queue.fetch_due(10, Utc::now()).await.expect("fetch");
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn mark_sending_does_not_count_attempt() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue.mark_sending(id).await.expect("mark sending");

        let loaded = queue.get(id).await.expect("get");
        assert_eq!(loaded.status, MailStatus::Sending);
        assert_eq!(loaded.attempts, 0);
    }

    #[tokio::test]
    async fn mark_sent_sets_status() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue.mark_sent(id).await.expect("mark sent");

        let loaded = queue.get(id).await.expect("get");
        assert_eq!(loaded.status, MailStatus::Sent);
    }

    #[tokio::test]
    async fn mark_failed_reschedules_into_future() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        let retry_at = Utc::now() + chrono::Duration::minutes(5);
        queue
            .mark_failed(id, "smtp timeout".into(), retry_at)
            .await
            .expect("mark failed");

        let loaded = queue.get(id).await.expect("get");
        assert_eq!(loaded.status, MailStatus::Failed);
        assert_eq!(loaded.attempts, 1);
        assert_eq!(loaded.last_error.as_deref(), Some("smtp timeout"));

        // Not yet due, so a fetch at "now" must skip it.
        let due_now = queue.fetch_due(10, Utc::now()).await.expect("fetch");
        assert!(due_now.is_empty());
        // Due again once the retry time has passed.
        let due_later = queue
            .fetch_due(10, retry_at + chrono::Duration::seconds(1))
            .await
            .expect("fetch");
        assert_eq!(due_later.len(), 1);
    }

    #[tokio::test]
    async fn mark_dead_counts_attempt_and_stays_out_of_queue() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue.mark_dead(id, "permanent".into()).await.expect("dead");

        let loaded = queue.get(id).await.expect("get");
        assert_eq!(loaded.status, MailStatus::Dead);
        assert_eq!(loaded.attempts, 1);

        let due = queue
            .fetch_due(10, Utc::now() + chrono::Duration::days(1))
            .await
            .expect("fetch");
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn requeue_sending_recovers_in_flight() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue.mark_sending(id).await.expect("mark sending");

        let reset = queue.requeue_sending().await.expect("requeue");
        assert_eq!(reset, 1);

        let due = queue.fetch_due(10, Utc::now()).await.expect("fetch");
        assert_eq!(due.len(), 1);
    }

    #[tokio::test]
    async fn requeue_stale_sending_only_resets_orphaned() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue.mark_sending(id).await.expect("mark sending");

        // Cutoff in the past: the in-flight mail is not yet stale.
        let reset = queue
            .requeue_stale_sending(Utc::now() - chrono::Duration::seconds(60))
            .await
            .expect("requeue");
        assert_eq!(reset, 0);

        // Cutoff in the future: the mail counts as orphaned and is reset.
        let reset = queue
            .requeue_stale_sending(Utc::now() + chrono::Duration::seconds(60))
            .await
            .expect("requeue");
        assert_eq!(reset, 1);
        assert_eq!(queue.get(id).await.expect("get").status, MailStatus::Pending);
    }

    #[tokio::test]
    async fn mark_unknown_returns_not_found() {
        let queue = memory_queue().await;
        let err = queue.mark_sent(Uuid::new_v4()).await.expect_err("not found");
        assert!(matches!(err, CoreError::NotFound(_)));
    }
}
