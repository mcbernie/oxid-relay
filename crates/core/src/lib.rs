//! Core domain logic for OxidRelay.
//!
//! This crate contains only business logic: message types, the transport
//! abstraction and the queue abstraction. It intentionally does not depend on
//! any concrete transport (SMTP, Graph, SES) or storage backend (SQLite).

pub mod error;
pub mod message;
pub mod queue;
pub mod transport;

pub use error::{CoreError, Result};
pub use message::{Address, Mail, MailId, MailStatus, NewMail};
pub use queue::Queue;
pub use transport::{Transport, TransportName};
