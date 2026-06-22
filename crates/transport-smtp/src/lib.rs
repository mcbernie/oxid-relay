//! SMTP transport backed by [`lettre`].
//!
//! Suitable for Microsoft 365 SMTP and any other STARTTLS capable SMTP server.
//! Connection settings come from the shared [`SmtpConfig`] in
//! `oxid_relay_core::config`.

use async_trait::async_trait;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use oxid_relay_core::config::SmtpConfig;
use oxid_relay_core::{Address, CoreError, Mail, Result, Transport};

/// SMTP transport implementation.
pub struct SmtpTransport {
    name: String,
    mailer: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpTransport {
    /// Builds a new SMTP transport from the shared configuration. The password
    /// is resolved from the configured environment variable.
    pub fn new(config: &SmtpConfig) -> Result<Self> {
        let password = config
            .password()
            .map_err(|err| CoreError::Transport(err.to_string()))?;
        let creds = Credentials::new(config.username.clone(), password);

        let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
            .map_err(|err| CoreError::Transport(err.to_string()))?
            .port(config.port)
            .credentials(creds)
            .build();

        Ok(Self {
            name: "smtp".to_string(),
            mailer,
        })
    }

    /// Converts a core [`Address`] into a lettre [`Mailbox`].
    fn mailbox(addr: &Address) -> Result<Mailbox> {
        let raw = match &addr.name {
            Some(name) => format!("{name} <{}>", addr.email),
            None => addr.email.clone(),
        };
        raw.parse::<Mailbox>()
            .map_err(|err| CoreError::InvalidMail(err.to_string()))
    }

    /// Builds a lettre [`Message`] (plain text) from a core [`Mail`].
    fn build_message(mail: &Mail) -> Result<Message> {
        let mut builder = Message::builder()
            .from(Self::mailbox(&mail.from)?)
            .subject(&mail.subject);

        for recipient in &mail.to {
            builder = builder.to(Self::mailbox(recipient)?);
        }

        builder
            .body(mail.body.clone())
            .map_err(|err| CoreError::InvalidMail(err.to_string()))
    }
}

#[async_trait]
impl Transport for SmtpTransport {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, mail: &Mail) -> Result<()> {
        let message = Self::build_message(mail)?;
        self.mailer
            .send(message)
            .await
            .map_err(|err| CoreError::Transport(err.to_string()))?;
        tracing::info!(mail_id = %mail.id, "mail delivered via smtp");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxid_relay_core::NewMail;
    use uuid::Uuid;

    fn sample_mail() -> Mail {
        Mail::from_new(
            NewMail {
                from: Address::new("relay@example.com"),
                to: vec![Address {
                    email: "ziel@example.com".into(),
                    name: Some("Zustellung".into()),
                }],
                subject: "Test".into(),
                body: "Grüße".into(),
                transport: None,
            },
            chrono::Utc::now(),
            Uuid::new_v4(),
        )
    }

    #[test]
    fn builds_message_from_mail() {
        let mail = sample_mail();
        assert!(SmtpTransport::build_message(&mail).is_ok());
    }

    #[test]
    fn mailbox_with_display_name_parses() {
        let addr = Address {
            email: "a@b.de".into(),
            name: Some("Name".into()),
        };
        assert!(SmtpTransport::mailbox(&addr).is_ok());
    }

    #[test]
    fn mailbox_plain_address_parses() {
        let addr = Address::new("a@b.de");
        assert!(SmtpTransport::mailbox(&addr).is_ok());
    }

    #[test]
    fn mailbox_rejects_garbage() {
        let addr = Address::new("not an address");
        assert!(matches!(
            SmtpTransport::mailbox(&addr),
            Err(CoreError::InvalidMail(_))
        ));
    }

    #[test]
    fn build_message_rejects_invalid_recipient() {
        let mut mail = sample_mail();
        mail.to = vec![Address::new("kein-at-zeichen")];
        assert!(matches!(
            SmtpTransport::build_message(&mail),
            Err(CoreError::InvalidMail(_))
        ));
    }
}
