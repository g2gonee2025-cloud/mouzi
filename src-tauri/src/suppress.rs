//! The watcher's self-suppression set: a bounded, expiring set of paths the
//! app has just moved, so the watcher can recognise its own events.
//!
//! **Module invariant: this file must not reference `crate::db`.** The set
//! holds a lock, and the app's whole lock story is that no database call is
//! ever made while this lock is held (the undo path depends on it — see
//! `undo_batch_holds_neither_lock_while_it_moves_a_file`). Keeping the database
//! out of this module makes that true by construction instead of by inspection.
//!
//! Every method takes `now` as a parameter rather than reading the clock, so
//! expiry is testable without sleeping and so one batch can be reasoned about
//! against a single instant.

use crate::safe_fs::normalize_lexically;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a guard entry lives.
///
/// This sizes event *delivery* latency, not move duration: the entry has to
/// outlive the gap between `fs::rename` returning and the OS handing the
/// resulting notification to our callback. 30 s is unchanged from the value this
/// map has always used — it was never the problem, and shortening it would
/// trade a real regression risk for nothing.
pub const SUPPRESSION_TTL: Duration = Duration::from_secs(30);

/// Bounds the map inside one TTL window.
///
/// The sweep alone bounds the *steady state* at `insert_rate * TTL` but not the
/// *peak*: a single `undo_all` over a large `action_logs` table arms two paths
/// per row for the whole batch, and under a recursive watch every organise move
/// arms two more. This cap exists to make that worst case finite and testable.
/// At roughly 120 bytes an entry, the ceiling is about 8 MB — worth paying to
/// make the bound a fact rather than an assumption about log-table size.
const MAX_ENTRIES: usize = 65_536;

#[derive(Debug, Clone, Copy)]
struct Suppression {
    armed_at: Instant,
    expires_at: Instant,
}

struct Inner {
    entries: HashMap<String, Suppression>,
}

pub struct SuppressionSet {
    inner: Mutex<Inner>,
}

/// The key a path is stored under, computed in exactly one place.
///
/// `PathBuf`'s own `Hash`/`Eq` are byte-wise, so they treat `C:\W\a.pdf` and
/// `c:\w\a.pdf` as two different paths and `a\..\b` as a third. Guarding one
/// spelling while the watcher reports another is a guard that silently does
/// nothing, so the key is normalised lexically and lowercased. This is strictly
/// *more* suppression than the raw-string key it replaces, and every arm site
/// arms a path the app itself built, so in practice the two agree.
fn key(path: &Path) -> String {
    normalize_lexically(path).to_string_lossy().to_lowercase()
}

impl SuppressionSet {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
            }),
        }
    }

    /// The map, recovering from poisoning instead of propagating it.
    ///
    /// A panic on any thread while this lock was held would otherwise make every
    /// later arm and lookup fail, permanently — turning one transient panic into
    /// a watcher that can no longer recognise its own moves, which is the exact
    /// failure this set exists to prevent. Recovery is safe here because the
    /// entries are independent: there is no whole-map invariant that a
    /// mid-operation panic could have left half-written.
    fn entries(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Insert or refresh guards for `paths`. Pure memory: no filesystem access,
    /// no other lock, safe to call immediately before a rename whose outcome is
    /// not yet known.
    pub fn arm(&self, paths: &[PathBuf], now: Instant, ttl: Duration) {
        if paths.is_empty() {
            return;
        }
        let mut inner = self.entries();
        for path in paths {
            inner.entries.insert(
                key(path),
                Suppression {
                    armed_at: now,
                    expires_at: now + ttl,
                },
            );
        }
    }

    /// One read-only lookup. This is the only method the notify callback calls.
    pub fn is_suppressed(&self, path: &Path, now: Instant) -> bool {
        let inner = self.entries();
        let Some(entry) = inner.entries.get(&key(path)) else {
            return false;
        };
        now < entry.expires_at
    }

    /// Drop expired entries; if still over `MAX_ENTRIES`, drop the
    /// newest-armed first. Returns how many were removed. No other lock, no I/O.
    ///
    /// Newest-armed first is deliberate and the opposite of the obvious choice.
    /// The event handler runs on the OS's thread, concurrently with whatever
    /// armed these entries, and has no ordering relationship to the arm order:
    /// an entry armed at the start of a large batch can have its event delivered
    /// after the batch finished. Oldest-first eviction would therefore destroy
    /// precisely the guards that are still needed, and would do so silently.
    pub fn sweep(&self, now: Instant) -> usize {
        let mut inner = self.entries();
        let before = inner.entries.len();
        inner.entries.retain(|_, entry| now < entry.expires_at);
        let mut removed = before - inner.entries.len();

        if inner.entries.len() > MAX_ENTRIES {
            let overflow = inner.entries.len() - MAX_ENTRIES;
            let mut newest_first: Vec<(&String, Instant)> = inner
                .entries
                .iter()
                .map(|(k, entry)| (k, entry.armed_at))
                .collect();
            newest_first.sort_unstable_by(|a, b| b.1.cmp(&a.1));
            // Owned before removing: the keys above borrow from the very map
            // being mutated.
            let doomed: Vec<String> = newest_first
                .into_iter()
                .take(overflow)
                .map(|(k, _)| k.clone())
                .collect();
            for key in &doomed {
                inner.entries.remove(key);
            }
            removed += doomed.len();
            // Observable on purpose. This branch is a safety net, and a safety
            // net nobody can see failing is not one — a silent cap here is
            // indistinguishable from "the guard map is fine".
            eprintln!(
                "[suppress] guard map hit its {MAX_ENTRIES}-entry cap; evicted {overflow} newest-armed entries"
            );
        }
        removed
    }

    /// (live entries, armed_at of the oldest) — for the size test.
    #[cfg(test)]
    pub fn stats(&self) -> (usize, Option<Instant>) {
        let inner = self.entries();
        let oldest = inner.entries.values().map(|e| e.armed_at).min();
        (inner.entries.len(), oldest)
    }

    /// Every live key, for tests that assert *which* paths were armed.
    #[cfg(test)]
    pub fn guarded_keys(&self) -> Vec<String> {
        self.entries().entries.keys().cloned().collect()
    }

    /// Whether `path` currently has a guard, ignoring expiry.
    ///
    /// `#[cfg(test)]` and not part of the production surface: the watcher never
    /// asks this question, because the watcher always has a `now` in hand and
    /// must respect the TTL. Tests that assert a guard was armed at a specific
    /// instant before a move do not have one.
    #[cfg(test)]
    pub fn is_guarded(&self, path: &str) -> bool {
        self.entries().entries.contains_key(&key(Path::new(path)))
    }

    /// Whether the lock is free right now.
    ///
    /// `#[cfg(test)]`, for the same reason as `is_guarded`: the lock-discipline
    /// tests assert that no lock is held at the instant a rename begins, and
    /// after the type swap they cannot reach the mutex directly.
    #[cfg(test)]
    pub fn is_lock_free(&self) -> bool {
        self.inner.try_lock().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// Instantiate the whole T3 table. Every assertion here is instant: `now`
    /// is a parameter, so no test sleeps and none of them can be flaky.
    #[test]
    fn t3_the_suppression_lease_arms_expires_normalises_and_bounds() {
        let set = SuppressionSet::new();
        let t0 = Instant::now();

        // Arm, then the armed path is suppressed.
        set.arm(&[p(r"C:\W\A.PDF")], t0, SUPPRESSION_TTL);
        assert!(set.is_suppressed(Path::new(r"C:\W\A.PDF"), t0));

        // Past the TTL it is not suppressed, and the sweep removes it. These
        // are two separate properties: a lookup must not keep honouring an
        // expired entry between sweeps, and a sweep must not keep an entry that
        // no lookup will ever consult again.
        let later = t0 + SUPPRESSION_TTL;
        assert!(!set.is_suppressed(Path::new(r"C:\W\A.PDF"), later));
        assert_eq!(set.sweep(later), 1);
        assert_eq!(set.stats().0, 0);

        // One entry, two spellings: casing must not create a second guard.
        set.arm(&[p(r"C:\W\A.PDF")], later, SUPPRESSION_TTL);
        assert!(set.is_suppressed(Path::new(r"c:\w\a.pdf"), later));
        assert_eq!(set.stats().0, 1, "a case variant must be the same key");

        // `a\..\b` and `b` are the same path, so one guard covers both.
        set.arm(&[p(r"C:\W\a\..\b.txt")], later, SUPPRESSION_TTL);
        assert!(set.is_suppressed(Path::new(r"C:\W\b.txt"), later));
        assert_eq!(set.stats().0, 2);

        // Re-arming refreshes rather than duplicating, and pushes the expiry out
        // — an undo that restores the same path twice must be guarded until the
        // second restore's events have been delivered, not until the first.
        set.arm(&[p(r"C:\W\A.PDF")], later + Duration::from_secs(10), SUPPRESSION_TTL);
        assert_eq!(set.stats().0, 2, "re-arming must not duplicate the entry");
        let midway = later + Duration::from_secs(10) + SUPPRESSION_TTL - Duration::from_secs(1);
        assert!(
            set.is_suppressed(Path::new(r"C:\W\A.PDF"), midway),
            "the refreshed guard expired at the original deadline"
        );
    }

    /// The bound is a property of the *type*, not of a caller remembering to
    /// sweep: any arm sequence followed by a sweep leaves the map at or under
    /// the cap.
    #[test]
    fn t3_sweep_bounds_the_map_and_evicts_the_newest_armed_first() {
        let set = SuppressionSet::new();
        let t0 = Instant::now();

        // Armed one at a time in ascending time order, so index 0 is the
        // oldest-armed entry and index MAX_ENTRIES is the newest-armed. Armed
        // individually rather than as one batch precisely so that "newest" is
        // unambiguous: a single batch arm would give every entry the same
        // `armed_at` and the eviction-order assertion below would be vacuous.
        let paths: Vec<PathBuf> = (0..=MAX_ENTRIES)
            .map(|i| p(&format!(r"C:\W\f{i}.bin")))
            .collect();
        for (i, path) in paths.iter().enumerate() {
            set.arm(
                std::slice::from_ref(path),
                t0 + Duration::from_micros(i as u64),
                SUPPRESSION_TTL,
            );
        }

        // Nothing has expired yet, so the cap is the only thing that can shrink
        // the map. One entry over the cap means exactly one eviction.
        let last_armed_at = t0 + Duration::from_micros(MAX_ENTRIES as u64);
        let evicted = set.sweep(last_armed_at);
        assert_eq!(evicted, 1);
        assert!(
            set.stats().0 <= MAX_ENTRIES,
            "sweep left {} entries, over the {MAX_ENTRIES} cap",
            set.stats().0
        );

        // The survivor is the oldest-armed path, and the newest-armed path is
        // the one gone. Inverting this is the bug the eviction order exists to
        // prevent: see `sweep`.
        assert!(
            set.is_guarded(r"C:\W\f0.bin"),
            "the oldest-armed guard was evicted first; that is the order that drops live guards"
        );
        assert!(
            !set.is_guarded(&format!(r"C:\W\f{MAX_ENTRIES}.bin")),
            "the newest-armed entry should have been the one evicted"
        );

        // And the map really did hold more than the cap beforehand, so the
        // assertion above is testing eviction and not a no-op sweep.
        assert_eq!(set.stats().0, MAX_ENTRIES);
    }
}
