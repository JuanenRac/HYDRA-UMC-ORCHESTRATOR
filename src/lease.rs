// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/lease.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! Leases: one name, one holder at a time, with a fencing token.
//!
//! Two watchdogs on the same cell (two controller boards, or a controller and a development machine) would both ask for the recovery of the
//! same node. A healer takes the lease `healer-of-<cell>` first and shows it with every recovery request; the orchestrator refuses the
//! recovery of anyone who does not hold it. The lease expires on its own when the holder stops renewing it, and every new grant carries a
//! token that is higher than every token handed out before, so a holder that was paused and woke up late is recognised as old.
//!
//! The table lives in memory on purpose: a lease lasts seconds, and after a restart the healers simply acquire it again. The token cannot go
//! back, because a new token is never lower than the clock in milliseconds.

use std::collections::BTreeMap;

/// The prefix of the leases that gate `POST /nodes/{node}/recover`.
pub const HEALER_PREFIX: &str = "healer-of-";
/// The most leases the table holds at once.
pub const MAX_LEASES: usize = 256;
/// The lease time asked for when the caller does not say.
pub const DEFAULT_TTL_MS: u64 = 15_000;
/// The shortest lease time: below it a renewal could never arrive in time.
pub const MIN_TTL_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub holder: String,
    pub token: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseError {
    /// The name or the holder is not usable.
    Invalid(String),
    /// Someone else holds a lease that has not expired.
    Held { holder: String, expires_at_ms: u64 },
    /// The caller is not the holder (wrong holder, wrong token, expired, or nobody holds it).
    NotHolder(String),
    /// The table is full.
    Full,
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::Invalid(why) => write!(f, "{why}"),
            LeaseError::Held { holder, .. } => write!(f, "the lease is held by {holder}"),
            LeaseError::NotHolder(why) => write!(f, "{why}"),
            LeaseError::Full => write!(f, "too many leases are held"),
        }
    }
}

/// A name or a holder: letters, digits and `. _ : -`, at most 128 characters.
pub fn valid_label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

#[derive(Debug, Default)]
pub struct LeaseTable {
    leases: BTreeMap<String, Lease>,
    last_token: u64,
}

impl LeaseTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn live(&self, name: &str, now_ms: u64) -> Option<&Lease> {
        self.leases.get(name).filter(|l| l.expires_at_ms > now_ms)
    }

    fn clamp_ttl(ttl_ms: Option<u64>, max_ttl_ms: u64) -> u64 {
        ttl_ms
            .unwrap_or(DEFAULT_TTL_MS)
            .clamp(MIN_TTL_MS, max_ttl_ms.max(MIN_TTL_MS))
    }

    /// Takes the lease when it is free, expired, or already the caller's (then it only gets longer and keeps its token).
    pub fn acquire(
        &mut self,
        name: &str,
        holder: &str,
        ttl_ms: Option<u64>,
        max_ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Lease, LeaseError> {
        if !valid_label(name) {
            return Err(LeaseError::Invalid(
                "the lease name must be 1 to 128 letters, digits or . _ : -".into(),
            ));
        }
        if !valid_label(holder) {
            return Err(LeaseError::Invalid(
                "the holder must be 1 to 128 letters, digits or . _ : -".into(),
            ));
        }
        let ttl = Self::clamp_ttl(ttl_ms, max_ttl_ms);
        if let Some(current) = self.live(name, now_ms) {
            if current.holder != holder {
                return Err(LeaseError::Held {
                    holder: current.holder.clone(),
                    expires_at_ms: current.expires_at_ms,
                });
            }
            let renewed = Lease {
                holder: current.holder.clone(),
                token: current.token,
                expires_at_ms: now_ms + ttl,
            };
            self.leases.insert(name.to_string(), renewed.clone());
            return Ok(renewed);
        }
        // drop what expired before counting, so a full table frees itself
        self.leases.retain(|_, l| l.expires_at_ms > now_ms);
        if !self.leases.contains_key(name) && self.leases.len() >= MAX_LEASES {
            return Err(LeaseError::Full);
        }
        let token = self.last_token.saturating_add(1).max(now_ms);
        self.last_token = token;
        let lease = Lease {
            holder: holder.to_string(),
            token,
            expires_at_ms: now_ms + ttl,
        };
        self.leases.insert(name.to_string(), lease.clone());
        Ok(lease)
    }

    /// Proof that `holder` holds `name` with `token` right now.
    pub fn check(
        &self,
        name: &str,
        holder: &str,
        token: u64,
        now_ms: u64,
    ) -> Result<&Lease, LeaseError> {
        match self.leases.get(name) {
            None => Err(LeaseError::NotHolder("nobody holds this lease".into())),
            Some(l) if l.expires_at_ms <= now_ms => {
                Err(LeaseError::NotHolder("the lease expired".into()))
            }
            Some(l) if l.holder != holder => Err(LeaseError::NotHolder(format!(
                "the lease is held by {}",
                l.holder
            ))),
            Some(l) if l.token != token => Err(LeaseError::NotHolder(
                "the token is not the current one".into(),
            )),
            Some(l) => Ok(l),
        }
    }

    /// Makes a lease the caller holds longer; the token stays.
    pub fn renew(
        &mut self,
        name: &str,
        holder: &str,
        token: u64,
        ttl_ms: Option<u64>,
        max_ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Lease, LeaseError> {
        self.check(name, holder, token, now_ms)?;
        let ttl = Self::clamp_ttl(ttl_ms, max_ttl_ms);
        let lease = self.leases.get_mut(name).expect("checked above");
        lease.expires_at_ms = now_ms + ttl;
        Ok(lease.clone())
    }

    /// Gives the lease up, so the next healer does not wait for it to expire.
    pub fn release(
        &mut self,
        name: &str,
        holder: &str,
        token: u64,
        now_ms: u64,
    ) -> Result<(), LeaseError> {
        self.check(name, holder, token, now_ms)?;
        self.leases.remove(name);
        Ok(())
    }

    /// True when some healer lease has not expired.
    pub fn healer_lease_active(&self, now_ms: u64) -> bool {
        self.leases
            .iter()
            .any(|(name, l)| name.starts_with(HEALER_PREFIX) && l.expires_at_ms > now_ms)
    }

    /// The leases that have not expired, by name.
    pub fn active(&self, now_ms: u64) -> Vec<(String, Lease)> {
        self.leases
            .iter()
            .filter(|(_, l)| l.expires_at_ms > now_ms)
            .map(|(n, l)| (n.clone(), l.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u64 = 60_000;

    #[test]
    fn a_free_lease_is_taken_and_a_second_holder_is_refused() {
        let mut t = LeaseTable::new();
        let a = t
            .acquire("healer-of-cell-1", "cm5", Some(5_000), MAX, 10_000)
            .unwrap();
        assert_eq!(a.expires_at_ms, 15_000);
        let err = t
            .acquire("healer-of-cell-1", "devhost", None, MAX, 11_000)
            .unwrap_err();
        assert_eq!(
            err,
            LeaseError::Held {
                holder: "cm5".into(),
                expires_at_ms: 15_000
            }
        );
    }

    #[test]
    fn the_holder_asking_again_keeps_its_token() {
        let mut t = LeaseTable::new();
        let a = t
            .acquire("healer-of-c", "cm5", Some(5_000), MAX, 10_000)
            .unwrap();
        let b = t
            .acquire("healer-of-c", "cm5", Some(5_000), MAX, 12_000)
            .unwrap();
        assert_eq!(a.token, b.token);
        assert_eq!(b.expires_at_ms, 17_000);
    }

    #[test]
    fn an_expired_lease_goes_to_the_next_holder_with_a_higher_token() {
        let mut t = LeaseTable::new();
        let a = t
            .acquire("healer-of-c", "cm5", Some(2_000), MAX, 10_000)
            .unwrap();
        let b = t
            .acquire("healer-of-c", "devhost", Some(2_000), MAX, 12_000)
            .unwrap();
        assert!(b.token > a.token);
        assert_eq!(b.holder, "devhost");
        // the old holder, waking up late, is no longer the holder
        assert!(t.check("healer-of-c", "cm5", a.token, 12_001).is_err());
        assert!(t.check("healer-of-c", "devhost", b.token, 12_001).is_ok());
    }

    #[test]
    fn tokens_never_go_down_even_if_the_clock_does() {
        let mut t = LeaseTable::new();
        let a = t
            .acquire("healer-of-c", "a", Some(1_000), MAX, 50_000)
            .unwrap();
        t.release("healer-of-c", "a", a.token, 50_001).unwrap();
        let b = t
            .acquire("healer-of-c", "b", Some(1_000), MAX, 10_000)
            .unwrap();
        assert!(b.token > a.token);
    }

    #[test]
    fn renew_needs_the_current_holder_and_token_and_does_not_revive_an_expired_lease() {
        let mut t = LeaseTable::new();
        let a = t
            .acquire("healer-of-c", "cm5", Some(2_000), MAX, 10_000)
            .unwrap();
        assert!(t
            .renew("healer-of-c", "other", a.token, None, MAX, 10_500)
            .is_err());
        assert!(t
            .renew("healer-of-c", "cm5", a.token + 1, None, MAX, 10_500)
            .is_err());
        let r = t
            .renew("healer-of-c", "cm5", a.token, Some(4_000), MAX, 11_000)
            .unwrap();
        assert_eq!(r.expires_at_ms, 15_000);
        assert!(t
            .renew("healer-of-c", "cm5", a.token, None, MAX, 15_000)
            .is_err());
    }

    #[test]
    fn release_frees_the_lease_at_once() {
        let mut t = LeaseTable::new();
        let a = t.acquire("healer-of-c", "cm5", None, MAX, 1_000).unwrap();
        assert!(t.release("healer-of-c", "other", a.token, 1_100).is_err());
        t.release("healer-of-c", "cm5", a.token, 1_100).unwrap();
        assert!(t
            .acquire("healer-of-c", "devhost", None, MAX, 1_200)
            .is_ok());
    }

    #[test]
    fn the_time_asked_for_is_kept_between_the_minimum_and_the_limit() {
        let mut t = LeaseTable::new();
        let short = t.acquire("a", "h", Some(1), MAX, 0).unwrap();
        assert_eq!(short.expires_at_ms, MIN_TTL_MS);
        let long = t.acquire("b", "h", Some(10_000_000), 30_000, 0).unwrap();
        assert_eq!(long.expires_at_ms, 30_000);
    }

    #[test]
    fn names_and_holders_are_checked() {
        let mut t = LeaseTable::new();
        assert!(matches!(
            t.acquire("", "h", None, MAX, 0),
            Err(LeaseError::Invalid(_))
        ));
        assert!(matches!(
            t.acquire("a b", "h", None, MAX, 0),
            Err(LeaseError::Invalid(_))
        ));
        assert!(matches!(
            t.acquire("a", "h/x", None, MAX, 0),
            Err(LeaseError::Invalid(_))
        ));
        assert!(matches!(
            t.acquire(&"x".repeat(129), "h", None, MAX, 0),
            Err(LeaseError::Invalid(_))
        ));
    }

    #[test]
    fn the_table_is_bounded_and_frees_what_expired() {
        let mut t = LeaseTable::new();
        for i in 0..MAX_LEASES {
            t.acquire(&format!("l{i}"), "h", Some(1_000), MAX, 0)
                .unwrap();
        }
        assert_eq!(
            t.acquire("extra", "h", None, MAX, 500),
            Err(LeaseError::Full)
        );
        assert!(t.acquire("extra", "h", None, MAX, 2_000).is_ok());
    }

    #[test]
    fn only_a_live_healer_lease_counts_as_active() {
        let mut t = LeaseTable::new();
        t.acquire("other-lease", "h", Some(5_000), MAX, 0).unwrap();
        assert!(!t.healer_lease_active(1_000));
        t.acquire("healer-of-c", "h", Some(2_000), MAX, 0).unwrap();
        assert!(t.healer_lease_active(1_000));
        assert!(!t.healer_lease_active(2_000));
    }

    #[test]
    fn the_active_leases_are_listed_by_name_and_without_the_expired_ones() {
        let mut t = LeaseTable::new();
        t.acquire("b-lease", "h2", Some(5_000), MAX, 0).unwrap();
        t.acquire("a-lease", "h1", Some(5_000), MAX, 0).unwrap();
        t.acquire("short", "h3", Some(1_000), MAX, 0).unwrap();
        let names: Vec<String> = t.active(500).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["a-lease", "b-lease", "short"]);
        let names: Vec<String> = t.active(1_000).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["a-lease", "b-lease"]);
        assert!(t.active(5_000).is_empty());
    }

    #[test]
    fn a_holder_can_renew_its_lease_when_the_table_is_full() {
        let mut t = LeaseTable::new();
        for i in 0..MAX_LEASES {
            t.acquire(&format!("l{i}"), "h", Some(30_000), MAX, 0)
                .unwrap();
        }
        // a newcomer is refused, but the holder of a lease still gets it longer and keeps its token
        assert_eq!(t.acquire("new", "h", None, MAX, 10), Err(LeaseError::Full));
        let before = t
            .check("l0", "h", t.active(10)[0].1.token, 10)
            .unwrap()
            .token;
        let renewed = t.acquire("l0", "h", Some(30_000), MAX, 10).unwrap();
        assert_eq!(renewed.token, before);
        assert_eq!(renewed.expires_at_ms, 30_010);
    }

    #[test]
    fn a_limit_below_the_minimum_still_gives_the_minimum_and_the_errors_say_why() {
        let mut t = LeaseTable::new();
        let l = t.acquire("a", "h", Some(60_000), 10, 100).unwrap();
        assert_eq!(l.expires_at_ms, 100 + MIN_TTL_MS);
        assert_eq!(
            t.check("nope", "h", 1, 0).unwrap_err().to_string(),
            "nobody holds this lease"
        );
        assert_eq!(
            t.check("a", "other", l.token, 200).unwrap_err().to_string(),
            "the lease is held by h"
        );
        assert_eq!(
            t.check("a", "h", l.token + 1, 200).unwrap_err().to_string(),
            "the token is not the current one"
        );
        assert_eq!(
            t.check("a", "h", l.token, l.expires_at_ms)
                .unwrap_err()
                .to_string(),
            "the lease expired"
        );
        assert_eq!(LeaseError::Full.to_string(), "too many leases are held");
        assert_eq!(
            t.acquire("a", "x", None, MAX, 200).unwrap_err().to_string(),
            "the lease is held by h"
        );
    }

    #[test]
    fn the_labels_accept_only_the_characters_that_are_safe_in_a_name() {
        let (long, too_long) = ("y".repeat(128), "y".repeat(129));
        for ok in ["healer-of-cell-1", "a.b_c:d-e", "X", long.as_str()] {
            assert!(valid_label(ok), "{ok}");
        }
        for bad in ["", "a b", "a/b", "a\n", "ñ", too_long.as_str()] {
            assert!(!valid_label(bad), "{bad:?}");
        }
    }
}
