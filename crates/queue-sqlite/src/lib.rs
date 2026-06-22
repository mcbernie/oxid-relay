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
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
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
        })
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
                 status, attempts, last_error, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .execute(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        Ok(mail)
    }

    async fn fetch_pending(&self, limit: u32) -> Result<Vec<Mail>> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM mails
            WHERE status IN ('pending', 'failed')
            ORDER BY created_at ASC
            LIMIT ?
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        rows.iter().map(Self::row_to_mail).collect()
    }

    async fn update_status(
        &self,
        id: MailId,
        status: MailStatus,
        last_error: Option<String>,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE mails
            SET status = ?,
                last_error = ?,
                attempts = attempts + 1,
                updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(status_to_str(status))
        .bind(last_error)
        .bind(Utc::now().to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|err| CoreError::Queue(err.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound(id.to_string()));
        }
        Ok(())
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
    async fn fetch_pending_returns_pending_and_failed() {
        let queue = memory_queue().await;
        queue.enqueue(sample()).await.expect("enqueue");

        let pending = queue.fetch_pending(10).await.expect("fetch");
        assert_eq!(pending.len(), 1);
    }

    #[tokio::test]
    async fn fetch_pending_skips_sent() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");
        queue
            .update_status(id, MailStatus::Sent, None)
            .await
            .expect("update");

        let pending = queue.fetch_pending(10).await.expect("fetch");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn update_status_marks_sent_and_counts_attempt() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        queue
            .update_status(id, MailStatus::Sent, None)
            .await
            .expect("update");

        let loaded = queue.get(id).await.expect("get");
        assert_eq!(loaded.status, MailStatus::Sent);
        assert_eq!(loaded.attempts, 1);
    }

    #[tokio::test]
    async fn update_status_records_error_for_failure() {
        let queue = memory_queue().await;
        let mail = sample();
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        queue
            .update_status(id, MailStatus::Failed, Some("smtp timeout".into()))
            .await
            .expect("update");

        let loaded = queue.get(id).await.expect("get");
        assert_eq!(loaded.status, MailStatus::Failed);
        assert_eq!(loaded.last_error.as_deref(), Some("smtp timeout"));
    }

    #[tokio::test]
    async fn update_status_unknown_returns_not_found() {
        let queue = memory_queue().await;
        let err = queue
            .update_status(Uuid::new_v4(), MailStatus::Sent, None)
            .await
            .expect_err("not found");
        assert!(matches!(err, CoreError::NotFound(_)));
    }
}
