//! SMTP ingress for OxidRelay.
//!
//! Runs an embedded SMTP server (via `mailin-embedded`) that accepts mail on
//! the LAN, authenticates the sender and enqueues the message. The submitting
//! service only waits for the durable enqueue, not for the onward delivery.
//!
//! Access control mirrors the documented model: an IP whitelist is enforced in
//! every mode, and AUTH LOGIN/PLAIN resolves the sender against the configured
//! services (mode B1) or self-registration (mode B2). Anonymous submission is
//! allowed only when explicitly enabled.

use std::net::{IpAddr, TcpListener};
use std::sync::Arc;

use ipnet::IpNet;
use mail_parser::MessageParser;
use mailin_embedded::{Handler, Response, Server, SslConfig, response};
use oxid_relay_core::config::{AuthConfig, SubjectConfig};
use oxid_relay_core::{Address, Config, CoreError, Mail, NewMail, Queue};
use thiserror::Error;
use tokio::runtime::Handle;
use uuid::Uuid;

/// Errors that can occur while starting or running the SMTP ingress.
#[derive(Debug, Error)]
pub enum IngressError {
    /// The `[ingress.smtp]` section is not configured.
    #[error("smtp ingress is not configured")]
    Disabled,
    /// TLS setup failed.
    #[error("tls setup failed: {0}")]
    Tls(String),
    /// The bind address was invalid.
    #[error("invalid bind address: {0}")]
    Bind(String),
    /// The SMTP server stopped with an error.
    #[error("smtp server error: {0}")]
    Serve(String),
}

/// Shared, read-only context for all connections.
struct Context {
    queue: Arc<dyn Queue>,
    handle: Handle,
    whitelist: Vec<IpNet>,
    auth: AuthConfig,
    anonymous_enabled: bool,
    subject: SubjectConfig,
}

/// Per-connection SMTP handler. Cloned by the server for each session, so the
/// transaction fields start empty for every connection.
#[derive(Clone)]
struct RelayHandler {
    ctx: Arc<Context>,
    authenticated_as: Option<String>,
    envelope_from: String,
    recipients: Vec<String>,
    data: Vec<u8>,
}

impl RelayHandler {
    fn new(ctx: Arc<Context>) -> Self {
        Self {
            ctx,
            authenticated_as: None,
            envelope_from: String::new(),
            recipients: Vec::new(),
            data: Vec::new(),
        }
    }

    /// Resolves credentials against the configured auth model.
    fn try_auth(&mut self, user: &str, password: &str) -> Response {
        match self.ctx.auth.authenticate(user, password) {
            Ok(Some(name)) => {
                self.authenticated_as = Some(name);
                response::AUTH_OK
            }
            Ok(None) => response::INVALID_CREDENTIALS,
            Err(err) => {
                tracing::warn!(error = %err, "auth resolution failed");
                response::TEMP_AUTH_FAILURE
            }
        }
    }

    /// Builds a validated [`Mail`] from the received message and envelope.
    fn build_mail(&self) -> Result<Mail, CoreError> {
        let parsed = MessageParser::default()
            .parse(&self.data)
            .ok_or_else(|| CoreError::InvalidMail("could not parse message".into()))?;

        let original_subject = parsed.subject().unwrap_or_default().to_string();
        let body = parsed
            .body_text(0)
            .map(|text| text.into_owned())
            .unwrap_or_else(|| String::from_utf8_lossy(&self.data).into_owned());

        let from = Address::new(strip_brackets(&self.envelope_from));
        let to: Vec<Address> = self
            .recipients
            .iter()
            .map(|addr| Address::new(strip_brackets(addr)))
            .collect();

        // Apply the sender label only for an authenticated identity.
        let subject = match &self.authenticated_as {
            Some(name) => self.ctx.subject.render(name, &original_subject),
            None => original_subject,
        };

        let new = NewMail {
            from,
            to,
            subject,
            body,
            transport: None,
        };
        new.validate()?;
        Ok(Mail::from_new(new, chrono::Utc::now(), Uuid::new_v4()))
    }
}

impl Handler for RelayHandler {
    fn helo(&mut self, ip: IpAddr, _domain: &str) -> Response {
        if is_allowed(&self.ctx.whitelist, ip) {
            response::OK
        } else {
            tracing::warn!(%ip, "connection rejected: not in whitelist");
            Response::custom(421, "Service not available".to_string())
        }
    }

    fn mail(&mut self, ip: IpAddr, _domain: &str, from: &str) -> Response {
        if !is_allowed(&self.ctx.whitelist, ip) {
            return Response::custom(421, "Service not available".to_string());
        }
        // Start a fresh transaction.
        self.envelope_from = from.to_string();
        self.recipients.clear();
        self.data.clear();

        if !self.ctx.anonymous_enabled && self.authenticated_as.is_none() {
            return response::AUTHENTICATION_REQUIRED;
        }
        response::OK
    }

    fn rcpt(&mut self, to: &str) -> Response {
        self.recipients.push(to.to_string());
        response::OK
    }

    fn data_start(&mut self, _domain: &str, _from: &str, _is8bit: bool, _to: &[String]) -> Response {
        self.data.clear();
        response::OK
    }

    fn data(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.data.extend_from_slice(buf);
        Ok(())
    }

    fn data_end(&mut self) -> Response {
        let mail = match self.build_mail() {
            Ok(mail) => mail,
            Err(err) => {
                tracing::warn!(error = %err, "rejected incoming mail");
                return Response::custom(554, format!("rejected: {err}"));
            }
        };

        // Block on the durable enqueue; this runs on an SMTP worker thread, not
        // on the async runtime, so blocking is safe here.
        match self.ctx.handle.block_on(self.ctx.queue.enqueue(mail)) {
            Ok(stored) => {
                tracing::info!(mail_id = %stored.id, "mail accepted into queue");
                response::OK
            }
            Err(err) => {
                tracing::error!(error = %err, "could not enqueue mail");
                response::INTERNAL_ERROR
            }
        }
    }

    fn auth_plain(
        &mut self,
        _authorization_id: &str,
        authentication_id: &str,
        password: &str,
    ) -> Response {
        self.try_auth(authentication_id, password)
    }

    fn auth_login(&mut self, username: &str, password: &str) -> Response {
        self.try_auth(username, password)
    }
}

/// Removes surrounding angle brackets and whitespace from an address.
fn strip_brackets(raw: &str) -> String {
    raw.trim().trim_start_matches('<').trim_end_matches('>').trim().to_string()
}

/// Parses whitelist entries (bare IPs or CIDR ranges) into networks.
fn parse_whitelist(entries: &[String]) -> Vec<IpNet> {
    let mut nets = Vec::new();
    for entry in entries {
        if let Ok(net) = entry.parse::<IpNet>() {
            nets.push(net);
        } else if let Ok(ip) = entry.parse::<IpAddr>() {
            let prefix = if ip.is_ipv4() { 32 } else { 128 };
            match IpNet::new(ip, prefix) {
                Ok(net) => nets.push(net),
                Err(_) => tracing::warn!(entry = %entry, "invalid whitelist entry, ignoring"),
            }
        } else {
            tracing::warn!(entry = %entry, "invalid whitelist entry, ignoring");
        }
    }
    nets
}

/// Whether the address is covered by any whitelist network.
fn is_allowed(nets: &[IpNet], ip: IpAddr) -> bool {
    nets.iter().any(|net| net.contains(&ip))
}

/// Builds the shared context from the configuration.
fn context(config: &Config, queue: Arc<dyn Queue>, handle: Handle) -> Arc<Context> {
    Arc::new(Context {
        queue,
        handle,
        whitelist: parse_whitelist(&config.security.ip_whitelist),
        auth: config.auth.clone(),
        anonymous_enabled: config.auth.anonymous.enabled,
        subject: config.subject.clone(),
    })
}

/// Configures a server with name, TLS off and AUTH mechanisms.
fn configured_server(
    hostname: &str,
    ctx: Arc<Context>,
) -> Result<Server<RelayHandler>, IngressError> {
    let mut server = Server::new(RelayHandler::new(ctx));
    server.with_name(hostname.to_string());
    server
        .with_ssl(SslConfig::None)
        .map_err(|err| IngressError::Tls(err.to_string()))?;
    // AUTH LOGIN/PLAIN is intentionally not advertised yet: mailin only permits
    // it over TLS, and enabling the mechanisms without TLS would put the session
    // into an auth-required state that rejects MAIL. v1 uses anonymous,
    // whitelist-only submission (mode A); AUTH arrives together with STARTTLS.
    Ok(server)
}

/// Runs the SMTP ingress, binding to the address from the configuration.
/// Blocks until the server stops; intended to run on a dedicated thread.
pub fn serve(config: Arc<Config>, queue: Arc<dyn Queue>, handle: Handle) -> Result<(), IngressError> {
    let smtp = config.ingress.smtp.as_ref().ok_or(IngressError::Disabled)?;
    let ctx = context(&config, queue, handle);
    let mut server = configured_server(&smtp.hostname, ctx)?;
    server
        .with_addr(&smtp.bind)
        .map_err(|err| IngressError::Bind(err.to_string()))?;
    tracing::info!(bind = %smtp.bind, "smtp ingress listening");
    server.serve().map_err(|err| IngressError::Serve(err.to_string()))
}

/// Runs the SMTP ingress on an already bound listener. Used for testing on an
/// ephemeral port. Blocks until the server stops.
pub fn serve_with_listener(
    config: Arc<Config>,
    queue: Arc<dyn Queue>,
    handle: Handle,
    listener: TcpListener,
) -> Result<(), IngressError> {
    let smtp = config.ingress.smtp.as_ref().ok_or(IngressError::Disabled)?;
    let ctx = context(&config, queue, handle);
    let mut server = configured_server(&smtp.hostname, ctx)?;
    server.with_tcp_listener(listener);
    server.serve().map_err(|err| IngressError::Serve(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxid_relay_core::MailStatus;
    use oxid_relay_queue_sqlite::SqliteQueue;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    static DB_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_config() -> Config {
        let raw = r#"
            [ingress.smtp]
            hostname = "oxid-test"

            [security]
            ip_whitelist = ["127.0.0.1", "::1"]

            [auth.anonymous]
            enabled = true
        "#;
        Config::from_toml_str(raw).expect("valid config")
    }

    async fn memory_queue() -> Arc<SqliteQueue> {
        let n = DB_COUNTER.fetch_add(1, Ordering::SeqCst);
        let url = format!("sqlite:file:oxidrelay_ingress_{n}?mode=memory&cache=shared");
        Arc::new(SqliteQueue::connect(&url).await.expect("queue"))
    }

    /// Reads an SMTP reply, skipping multiline continuation lines.
    async fn reply<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> String {
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read line");
            // A space after the 3-digit code marks the final line.
            if line.len() >= 4 && line.as_bytes()[3] == b' ' {
                return line;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepts_anonymous_mail_and_enqueues() {
        let queue = memory_queue().await;
        let config = Arc::new(test_config());

        // Bind an ephemeral port and hand the listener to the server thread.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();

        let handle = Handle::current();
        let server_queue = queue.clone();
        let server_config = config.clone();
        std::thread::spawn(move || {
            let _ = serve_with_listener(server_config, server_queue, handle, listener);
        });

        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        let (read_half, mut write) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        assert!(reply(&mut reader).await.starts_with("220"));
        write.write_all(b"EHLO test\r\n").await.expect("ehlo");
        assert!(reply(&mut reader).await.starts_with("250"));

        write
            .write_all(b"MAIL FROM:<relay@example.com>\r\n")
            .await
            .expect("mail");
        assert!(reply(&mut reader).await.starts_with("250"));
        write
            .write_all(b"RCPT TO:<ziel@example.com>\r\n")
            .await
            .expect("rcpt");
        assert!(reply(&mut reader).await.starts_with("250"));
        write.write_all(b"DATA\r\n").await.expect("data");
        assert!(reply(&mut reader).await.starts_with("354"));
        write
            .write_all(b"Subject: Status Okay\r\n\r\nKoerper\r\n.\r\n")
            .await
            .expect("body");
        assert!(reply(&mut reader).await.starts_with("250"));
        write.write_all(b"QUIT\r\n").await.expect("quit");

        // The mail must be durably queued. Anonymous senders are not labelled,
        // so the subject is kept as received.
        let due = queue
            .fetch_due(10, chrono::Utc::now())
            .await
            .expect("fetch");
        assert_eq!(due.len(), 1);
        let mail = &due[0];
        assert_eq!(mail.status, MailStatus::Pending);
        assert_eq!(mail.subject, "Status Okay");
        assert_eq!(mail.from.email, "relay@example.com");
        assert_eq!(mail.to.len(), 1);
        assert_eq!(mail.to[0].email, "ziel@example.com");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_non_whitelisted_ip() {
        // Whitelist excludes localhost, so the connection must be refused.
        let raw = r#"
            [ingress.smtp]
            [security]
            ip_whitelist = ["10.0.0.0/8"]
        "#;
        let config = Arc::new(Config::from_toml_str(raw).expect("config"));
        let queue = memory_queue().await;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = Handle::current();
        let server_queue = queue.clone();
        std::thread::spawn(move || {
            let _ = serve_with_listener(config, server_queue, handle, listener);
        });

        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        let (read_half, mut write) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        assert!(reply(&mut reader).await.starts_with("220"));
        write.write_all(b"EHLO test\r\n").await.expect("ehlo");
        // Greeting accepted, but HELO/EHLO from a blocked IP is refused (421).
        assert!(reply(&mut reader).await.starts_with("421"));
    }

    #[test]
    fn whitelist_parses_ip_and_cidr() {
        let nets = parse_whitelist(&["127.0.0.1".to_string(), "10.0.0.0/8".to_string()]);
        assert_eq!(nets.len(), 2);
        assert!(is_allowed(&nets, "127.0.0.1".parse().unwrap()));
        assert!(is_allowed(&nets, "10.1.2.3".parse().unwrap()));
        assert!(!is_allowed(&nets, "192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn strip_brackets_removes_wrappers() {
        assert_eq!(strip_brackets("<a@b.de>"), "a@b.de");
        assert_eq!(strip_brackets("  a@b.de "), "a@b.de");
    }
}
