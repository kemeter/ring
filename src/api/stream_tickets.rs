//! In-memory tickets for SSE / WebSocket auth.
//!
//! Browsers can't set custom headers on `EventSource`, so we mint a short-
//! lived ticket from a regular Bearer-authenticated call and let the client
//! pass it as `?ticket=` on the streaming URL. The ticket expires after 30s
//! and is bound to a specific scope (e.g. `deployment:logs:<id>`), so even
//! if it leaks into an access log it's effectively dead on arrival.
//!
//! The ticket is *reusable* within its TTL window. This is intentional:
//! EventSource auto-reconnects with the same URL on transient network
//! failures, so a strict single-use would break reconnection. The TTL is
//! short enough that the security loss vs single-use is negligible.
//!
//! No persistence — a server restart invalidates outstanding tickets, which
//! is fine because the SSE connections they protect die at the same moment.

use rand::Rng;
use rand::distr::Alphanumeric;
use rand::rng;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TICKET_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct Ticket {
    /// User who minted the ticket. Kept for audit logging and future
    /// periodic re-authorization inside long-lived streams.
    #[allow(dead_code)]
    pub(crate) user_id: String,
    pub(crate) scope: String,
    expires_at: Instant,
}

#[derive(Clone, Default)]
pub(crate) struct TicketStore {
    inner: Arc<Mutex<HashMap<String, Ticket>>>,
}

impl TicketStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh single-use ticket for `user_id` valid for `scope`.
    /// Returns the opaque token to hand back to the client.
    pub(crate) fn mint(&self, user_id: String, scope: String) -> String {
        let token = generate_token();
        let mut map = self.inner.lock().unwrap();
        self.purge_expired(&mut map);
        map.insert(
            token.clone(),
            Ticket {
                user_id,
                scope,
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        token
    }

    /// Validate a ticket. Returns the ticket payload if it exists, isn't
    /// expired, and matches `expected_scope`. The ticket stays in the store
    /// — multiple reads are allowed within the TTL to keep EventSource
    /// auto-reconnect working. The TTL is short enough that re-use within
    /// the window is not meaningfully weaker than single-use.
    pub(crate) fn consume(&self, token: &str, expected_scope: &str) -> Option<Ticket> {
        let mut map = self.inner.lock().unwrap();
        self.purge_expired(&mut map);
        let ticket = map.get(token)?;
        if ticket.expires_at <= Instant::now() {
            return None;
        }
        if ticket.scope != expected_scope {
            return None;
        }
        Some(ticket.clone())
    }

    fn purge_expired(&self, map: &mut HashMap<String, Ticket>) {
        let now = Instant::now();
        map.retain(|_, t| t.expires_at > now);
    }
}

fn generate_token() -> String {
    format!(
        "tk_stream_{}",
        rng()
            .sample_iter(&Alphanumeric)
            .take(48)
            .map(char::from)
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_then_consume_matches() {
        let store = TicketStore::new();
        let t = store.mint("u1".into(), "logs:abc".into());
        let got = store.consume(&t, "logs:abc").unwrap();
        assert_eq!(got.user_id, "u1");
    }

    #[test]
    fn ticket_is_reusable_within_ttl() {
        let store = TicketStore::new();
        let t = store.mint("u1".into(), "logs:abc".into());
        // EventSource auto-reconnect needs to replay the same URL; two
        // consecutive consumes within the TTL window must both succeed.
        assert!(store.consume(&t, "logs:abc").is_some());
        assert!(store.consume(&t, "logs:abc").is_some());
    }

    #[test]
    fn wrong_scope_is_rejected_but_ticket_stays() {
        let store = TicketStore::new();
        let t = store.mint("u1".into(), "logs:abc".into());
        assert!(store.consume(&t, "logs:xyz").is_none());
        // The legitimate scope still works after a probe with the wrong one.
        assert!(store.consume(&t, "logs:abc").is_some());
    }

    #[test]
    fn unknown_token_returns_none() {
        let store = TicketStore::new();
        assert!(store.consume("nope", "logs:abc").is_none());
    }
}
