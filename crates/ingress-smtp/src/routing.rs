//! Routing decision for an incoming mail, including a check that every
//! target transport is actually available in this process.

use std::collections::HashSet;

use oxid_relay_core::config::{Route, RouteTarget, RoutingConfig};

/// Which transports exist in the running relay. Transports whose plugin was
/// skipped at startup (e.g. a missing secret) are absent here.
#[derive(Debug, Clone, Default)]
pub struct TransportAvailability {
    /// Names of the transports that were built successfully.
    available: HashSet<String>,
    /// Transport the dispatcher falls back to for mails without one.
    default: Option<String>,
}

impl TransportAvailability {
    /// Creates the availability view from the built transport names and the
    /// default transport name (if any).
    pub fn new(available: impl IntoIterator<Item = String>, default: Option<String>) -> Self {
        Self {
            available: available.into_iter().collect(),
            default,
        }
    }

    /// Whether `name` can deliver mail. An empty name means "use the default
    /// transport"; without a configured default there is nothing to check, so
    /// the mail is let through for the dispatcher to judge.
    fn is_available(&self, name: &str) -> bool {
        let effective = if name.is_empty() {
            match &self.default {
                Some(default) => default.as_str(),
                None => return true,
            }
        } else {
            name
        };
        self.available.contains(effective)
    }

    /// The transport name to report for a target (resolving the default).
    fn effective_name<'a>(&'a self, name: &'a str) -> &'a str {
        if name.is_empty() {
            self.default.as_deref().unwrap_or(name)
        } else {
            name
        }
    }
}

/// Outcome of planning the delivery of one mail.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// The routing policy refuses this sender (permanent failure, 550).
    Reject,
    /// A target transport is unavailable (temporary failure, 451). Carries the
    /// name of the first unavailable transport.
    Unavailable(String),
    /// Enqueue one mail per target. A target with an empty transport name
    /// means "dispatcher default".
    Deliver(Vec<RouteTarget>),
}

/// Plans delivery for `sender`. With inactive routing a single default-transport
/// delivery is planned. For fan-out, a single unavailable target makes the
/// whole mail `Unavailable`: nothing is enqueued, so the sender's retry cannot
/// duplicate the deliveries that would have succeeded.
pub fn plan(routing: &RoutingConfig, availability: &TransportAvailability, sender: &str) -> Plan {
    let targets = if routing.is_active() {
        match routing.resolve(sender) {
            Route::Reject => return Plan::Reject,
            Route::Deliver(targets) => targets,
        }
    } else {
        vec![RouteTarget {
            transport: String::new(),
            recipients: None,
        }]
    };
    match targets
        .iter()
        .find(|target| !availability.is_available(&target.transport))
    {
        Some(target) => {
            Plan::Unavailable(availability.effective_name(&target.transport).to_string())
        }
        None => Plan::Deliver(targets),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxid_relay_core::Config;

    fn routing(raw: &str) -> RoutingConfig {
        Config::from_toml_str(raw).expect("config").routing
    }

    fn avail(names: &[&str], default: Option<&str>) -> TransportAvailability {
        TransportAvailability::new(
            names.iter().map(|n| n.to_string()),
            default.map(str::to_string),
        )
    }

    #[test]
    fn available_target_is_delivered() {
        let r = routing("[routing.senders.\"a@x\"]\ntransport = \"teams\"");
        let plan = plan(&r, &avail(&["teams"], None), "a@x");
        assert!(matches!(plan, Plan::Deliver(t) if t.len() == 1));
    }

    #[test]
    fn missing_target_is_unavailable() {
        let r = routing("[routing.senders.\"a@x\"]\ntransport = \"teams\"");
        assert_eq!(
            plan(&r, &avail(&["graph"], Some("graph")), "a@x"),
            Plan::Unavailable("teams".into())
        );
    }

    #[test]
    fn partial_fan_out_is_unavailable() {
        let r = routing(
            "[routing.senders.\"a@x\"]\ntargets = [{ transport = \"graph\" }, { transport = \"teams\" }]",
        );
        assert_eq!(
            plan(&r, &avail(&["graph"], None), "a@x"),
            Plan::Unavailable("teams".into())
        );
    }

    #[test]
    fn reject_stays_reject() {
        let r = routing("[routing.senders.\"a@x\"]\ntransport = \"teams\"");
        assert_eq!(plan(&r, &avail(&[], None), "other@x"), Plan::Reject);
    }

    #[test]
    fn inactive_routing_checks_default_transport() {
        let r = RoutingConfig::default();
        assert_eq!(
            plan(&r, &avail(&["smtp"], Some("graph")), "a@x"),
            Plan::Unavailable("graph".into())
        );
        assert!(matches!(
            plan(&r, &avail(&["graph"], Some("graph")), "a@x"),
            Plan::Deliver(_)
        ));
        // No default configured: dispatcher decides.
        assert!(matches!(
            plan(&r, &avail(&[], None), "a@x"),
            Plan::Deliver(_)
        ));
    }
}
