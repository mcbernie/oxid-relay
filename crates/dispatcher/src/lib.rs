//! Background dispatcher: drains the queue and delivers mails in parallel.
//!
//! The dispatcher knows only the [`Queue`] and [`Transport`] traits, never a
//! concrete backend. It polls for due mails, claims each one, and delivers them
//! concurrently up to a configured limit. Failures are rescheduled with
//! exponential backoff until exhausted, then buried as dead.

mod retry;

pub use retry::RetryPolicy;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use oxid_relay_core::{Mail, Queue, Transport};
use tokio::sync::Semaphore;

/// Tuning knobs for the dispatcher.
#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    /// Maximum mails fetched per polling tick.
    pub batch_size: u32,
    /// Maximum mails delivered concurrently.
    pub concurrency: usize,
    /// Delay between polling ticks.
    pub poll_interval: Duration,
    /// Retry behaviour for failed deliveries.
    pub retry: RetryPolicy,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            concurrency: 8,
            poll_interval: Duration::from_secs(5),
            retry: RetryPolicy::default(),
        }
    }
}

struct Inner {
    queue: Arc<dyn Queue>,
    transports: HashMap<String, Arc<dyn Transport>>,
    default_transport: Option<String>,
    config: DispatcherConfig,
}

/// Drains the queue and delivers mails in parallel.
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<Inner>,
}

impl Dispatcher {
    /// Builds a dispatcher from a queue, the available transports keyed by name,
    /// an optional default transport and tuning configuration.
    pub fn new(
        queue: Arc<dyn Queue>,
        transports: HashMap<String, Arc<dyn Transport>>,
        default_transport: Option<String>,
        config: DispatcherConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                queue,
                transports,
                default_transport,
                config,
            }),
        }
    }

    /// Resets mails left in-flight by a previous crash back to pending. Call
    /// once at startup.
    pub async fn recover(&self) {
        match self.inner.queue.requeue_sending().await {
            Ok(0) => {}
            Ok(count) => tracing::warn!(count, "recovered in-flight mails to pending"),
            Err(err) => tracing::error!(error = %err, "failed to recover in-flight mails"),
        }
    }

    /// Runs one polling tick: fetch due mails and deliver them in parallel.
    /// Returns the number of mails processed.
    pub async fn tick(&self) -> usize {
        let now = Utc::now();
        let due = match self
            .inner
            .queue
            .fetch_due(self.inner.config.batch_size, now)
            .await
        {
            Ok(due) => due,
            Err(err) => {
                tracing::error!(error = %err, "fetch_due failed");
                return 0;
            }
        };
        if due.is_empty() {
            return 0;
        }

        let semaphore = Arc::new(Semaphore::new(self.inner.config.concurrency));
        let mut handles = Vec::with_capacity(due.len());

        for mail in due {
            // Claim the mail so the next tick does not pick it up again.
            if let Err(err) = self.inner.queue.mark_sending(mail.id).await {
                tracing::error!(error = %err, mail_id = %mail.id, "could not claim mail");
                continue;
            }
            // Bound the number of concurrent deliveries.
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => break,
            };
            let inner = self.inner.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit;
                deliver(inner, mail).await;
            }));
        }

        let processed = handles.len();
        for handle in handles {
            let _ = handle.await;
        }
        processed
    }

    /// Runs the dispatcher loop until `shutdown` resolves.
    pub async fn run(&self, shutdown: impl std::future::Future<Output = ()>) {
        self.recover().await;
        let mut ticker = tokio::time::interval(self.inner.config.poll_interval);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    tracing::info!("dispatcher shutting down");
                    break;
                }
                _ = ticker.tick() => {
                    let processed = self.tick().await;
                    if processed > 0 {
                        tracing::debug!(processed, "dispatcher tick");
                    }
                }
            }
        }
    }
}

/// Delivers a single mail and records the outcome in the queue.
async fn deliver(inner: Arc<Inner>, mail: Mail) {
    let transport_name = mail
        .transport
        .clone()
        .or_else(|| inner.default_transport.clone());

    let transport = match transport_name
        .as_ref()
        .and_then(|name| inner.transports.get(name))
    {
        Some(transport) => transport.clone(),
        None => {
            let message = format!(
                "no transport available (requested {:?}, default {:?})",
                mail.transport, inner.default_transport
            );
            tracing::error!(mail_id = %mail.id, "{message}");
            let _ = inner.queue.mark_dead(mail.id, message).await;
            return;
        }
    };

    match transport.send(&mail).await {
        Ok(()) => {
            if let Err(err) = inner.queue.mark_sent(mail.id).await {
                tracing::error!(error = %err, mail_id = %mail.id, "could not mark mail as sent");
            }
        }
        Err(err) => {
            let attempts = mail.attempts + 1;
            let message = err.to_string();
            if inner.config.retry.is_exhausted(attempts) {
                tracing::warn!(mail_id = %mail.id, attempts, error = %message, "mail permanently failed");
                let _ = inner.queue.mark_dead(mail.id, message).await;
            } else {
                let delay = inner.config.retry.backoff(attempts);
                let retry_at = Utc::now()
                    + chrono::Duration::from_std(delay)
                        .unwrap_or_else(|_| chrono::Duration::seconds(60));
                tracing::warn!(mail_id = %mail.id, attempts, error = %message, "delivery failed, scheduling retry");
                let _ = inner.queue.mark_failed(mail.id, message, retry_at).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use oxid_relay_core::{Address, CoreError, MailId, MailStatus, NewMail, Result};
    use oxid_relay_queue_sqlite::SqliteQueue;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    static DB_COUNTER: AtomicUsize = AtomicUsize::new(0);

    async fn memory_queue() -> Arc<SqliteQueue> {
        let n = DB_COUNTER.fetch_add(1, Ordering::SeqCst);
        let url = format!("sqlite:file:oxidrelay_disp_{n}?mode=memory&cache=shared");
        Arc::new(SqliteQueue::connect(&url).await.expect("queue"))
    }

    fn mail_for(transport: Option<&str>) -> Mail {
        Mail::from_new(
            NewMail {
                from: Address::new("relay@example.com"),
                to: vec![Address::new("ziel@example.com")],
                subject: "Status".into(),
                body: "Okay".into(),
                transport: transport.map(|t| t.to_string()),
            },
            Utc::now(),
            Uuid::new_v4(),
        )
    }

    struct OkTransport;
    #[async_trait]
    impl Transport for OkTransport {
        fn name(&self) -> &str {
            "smtp"
        }
        async fn send(&self, _mail: &Mail) -> Result<()> {
            Ok(())
        }
    }

    struct FailTransport;
    #[async_trait]
    impl Transport for FailTransport {
        fn name(&self) -> &str {
            "smtp"
        }
        async fn send(&self, _mail: &Mail) -> Result<()> {
            Err(CoreError::Transport("boom".into()))
        }
    }

    struct SlowTransport {
        current: Arc<AtomicUsize>,
        max: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Transport for SlowTransport {
        fn name(&self) -> &str {
            "smtp"
        }
        async fn send(&self, _mail: &Mail) -> Result<()> {
            let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            self.current.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn transports(transport: Arc<dyn Transport>) -> HashMap<String, Arc<dyn Transport>> {
        let mut map = HashMap::new();
        map.insert("smtp".to_string(), transport);
        map
    }

    async fn status_of(queue: &SqliteQueue, id: MailId) -> Mail {
        queue.get(id).await.expect("get mail")
    }

    #[tokio::test]
    async fn delivers_and_marks_sent() {
        let queue = memory_queue().await;
        let mail = mail_for(Some("smtp"));
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        let dispatcher = Dispatcher::new(
            queue.clone(),
            transports(Arc::new(OkTransport)),
            None,
            DispatcherConfig::default(),
        );
        let processed = dispatcher.tick().await;
        assert_eq!(processed, 1);
        assert_eq!(status_of(&queue, id).await.status, MailStatus::Sent);
    }

    #[tokio::test]
    async fn uses_default_transport_when_unset() {
        let queue = memory_queue().await;
        let mail = mail_for(None);
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        let dispatcher = Dispatcher::new(
            queue.clone(),
            transports(Arc::new(OkTransport)),
            Some("smtp".to_string()),
            DispatcherConfig::default(),
        );
        dispatcher.tick().await;
        assert_eq!(status_of(&queue, id).await.status, MailStatus::Sent);
    }

    #[tokio::test]
    async fn unknown_transport_is_buried() {
        let queue = memory_queue().await;
        let mail = mail_for(Some("does-not-exist"));
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        let dispatcher = Dispatcher::new(
            queue.clone(),
            transports(Arc::new(OkTransport)),
            None,
            DispatcherConfig::default(),
        );
        dispatcher.tick().await;
        assert_eq!(status_of(&queue, id).await.status, MailStatus::Dead);
    }

    #[tokio::test]
    async fn failure_reschedules_with_backoff() {
        let queue = memory_queue().await;
        let mail = mail_for(Some("smtp"));
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        let dispatcher = Dispatcher::new(
            queue.clone(),
            transports(Arc::new(FailTransport)),
            None,
            DispatcherConfig::default(),
        );
        dispatcher.tick().await;

        let loaded = status_of(&queue, id).await;
        assert_eq!(loaded.status, MailStatus::Failed);
        assert_eq!(loaded.attempts, 1);
        assert!(loaded.next_attempt_at > Utc::now());

        // Not due now, so a second tick does nothing.
        assert_eq!(dispatcher.tick().await, 0);
    }

    #[tokio::test]
    async fn exhausted_attempts_are_buried() {
        let queue = memory_queue().await;
        let mail = mail_for(Some("smtp"));
        let id = mail.id;
        queue.enqueue(mail).await.expect("enqueue");

        let config = DispatcherConfig {
            retry: RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::default()
            },
            ..DispatcherConfig::default()
        };
        let dispatcher =
            Dispatcher::new(queue.clone(), transports(Arc::new(FailTransport)), None, config);
        dispatcher.tick().await;

        let loaded = status_of(&queue, id).await;
        assert_eq!(loaded.status, MailStatus::Dead);
        assert_eq!(loaded.attempts, 1);
    }

    #[tokio::test]
    async fn deliveries_run_in_parallel() {
        let queue = memory_queue().await;
        for _ in 0..4 {
            queue.enqueue(mail_for(Some("smtp"))).await.expect("enqueue");
        }

        let current = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let transport: Arc<dyn Transport> = Arc::new(SlowTransport {
            current: current.clone(),
            max: max.clone(),
        });

        let dispatcher = Dispatcher::new(
            queue.clone(),
            transports(transport),
            None,
            DispatcherConfig {
                concurrency: 4,
                ..DispatcherConfig::default()
            },
        );
        let processed = dispatcher.tick().await;
        assert_eq!(processed, 4);
        // All four ran concurrently rather than one after another.
        assert!(
            max.load(Ordering::SeqCst) >= 2,
            "expected parallel delivery, observed max concurrency {}",
            max.load(Ordering::SeqCst)
        );
    }
}
