//! Mail message types and their lifecycle state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{CoreError, Result};

/// Unique identifier of a queued mail.
pub type MailId = Uuid;

/// A single mail address with an optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    /// The raw e-mail address, e.g. `relay@example.com`.
    pub email: String,
    /// Optional human readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Address {
    /// Creates a new address from an e-mail string.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            name: None,
        }
    }
}

/// Delivery state of a mail inside the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailStatus {
    /// Waiting to be sent.
    Pending,
    /// Currently being delivered by a transport.
    Sending,
    /// Successfully delivered.
    Sent,
    /// Delivery failed but may be retried.
    Failed,
    /// Permanently failed after exhausting all retries.
    Dead,
}

/// Payload used to enqueue a new mail. Created by the API layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMail {
    /// Sender address.
    pub from: Address,
    /// One or more recipients.
    pub to: Vec<Address>,
    /// Mail subject.
    pub subject: String,
    /// Plain text body.
    pub body: String,
    /// Optional name of the transport to use. Falls back to the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

impl NewMail {
    /// Validates the payload before it is turned into a queued [`Mail`].
    pub fn validate(&self) -> Result<()> {
        if self.from.email.trim().is_empty() {
            return Err(CoreError::InvalidMail("sender address is empty".into()));
        }
        if self.to.is_empty() {
            return Err(CoreError::InvalidMail("no recipients given".into()));
        }
        if self.subject.trim().is_empty() {
            return Err(CoreError::InvalidMail("subject is empty".into()));
        }
        Ok(())
    }
}

/// A mail as stored in the queue, including delivery metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mail {
    /// Unique identifier.
    pub id: MailId,
    /// Sender address.
    pub from: Address,
    /// Recipients.
    pub to: Vec<Address>,
    /// Mail subject.
    pub subject: String,
    /// Plain text body.
    pub body: String,
    /// Transport this mail is routed to, if explicitly chosen.
    pub transport: Option<String>,
    /// Current delivery state.
    pub status: MailStatus,
    /// Number of delivery attempts performed so far.
    pub attempts: u32,
    /// Last error message, if delivery failed.
    pub last_error: Option<String>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Timestamp of the last update.
    pub updated_at: DateTime<Utc>,
    /// Earliest time the mail may be (re-)attempted. Set into the future on
    /// failure to implement backoff; equals `created_at` for a fresh mail.
    pub next_attempt_at: DateTime<Utc>,
}

impl Mail {
    /// Builds a queued mail from a validated payload at the given time.
    pub fn from_new(new: NewMail, now: DateTime<Utc>, id: MailId) -> Self {
        Self {
            id,
            from: new.from,
            to: new.to,
            subject: new.subject,
            body: new.body,
            transport: new.transport,
            status: MailStatus::Pending,
            attempts: 0,
            last_error: None,
            created_at: now,
            updated_at: now,
            next_attempt_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(to: Vec<Address>, subject: &str) -> NewMail {
        NewMail {
            from: Address::new("relay@example.com"),
            to,
            subject: subject.into(),
            body: "Hallo Welt".into(),
            transport: None,
        }
    }

    #[test]
    fn validate_accepts_complete_mail() {
        let mail = sample(vec![Address::new("ziel@example.com")], "Test");
        assert!(mail.validate().is_ok());
    }

    #[test]
    fn validate_rejects_missing_recipients() {
        let mail = sample(vec![], "Test");
        assert!(matches!(mail.validate(), Err(CoreError::InvalidMail(_))));
    }

    #[test]
    fn validate_rejects_empty_subject() {
        let mail = sample(vec![Address::new("ziel@example.com")], "  ");
        assert!(matches!(mail.validate(), Err(CoreError::InvalidMail(_))));
    }
}
