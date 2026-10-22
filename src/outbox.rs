// HYDRA-UMC-ORCHESTRATOR - src/outbox.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
//
// server.rs's
// own `reconcile_pending_remote_closes()` already retries confirming a
// mission's terminal outcome to Job-Dispatcher - but its own worklist
// (`MissionRegistry::pending_remote_closes()`) was purely in-memory,
// same as every other mission this process knows about (see mission.rs's
// own module doc). A real process restart between a mission's local
// terminal transition and Job-Dispatcher's own ACK used to lose the
// pending intent entirely along with the mission itself - Job-Dispatcher
// could be left believing a robot's reservation was still active
// forever, with no path back to consistency short of a human noticing.
//
// This is the real, durable "outbox" the review's own proposed fix names:
// one small JSON file recording exactly the pending remote-close intents
// (mission id + which terminal outcome to report), independent of
// MissionRegistry's own ephemeral state - survives a restart on its own,
// and is what server.rs's own reconciliation now walks as its real
// worklist, both immediately at startup and on its usual periodic retry.
// Deliberately scoped to ONLY this one real durability gap, not a full
// MissionRegistry persistence layer (a separate, much larger concern
// this finding does not ask for): a mission that never had a pending
// remote close needs nothing durable here, by design.
//
// Same real crash-safe file pattern as HYDRA-UMC-SWARM-SYNC's own
// store.rs: a missing file is a real, honest "nothing pending yet" (a
// fresh deployment, or every prior intent already confirmed), not an
// error; every write goes to a sibling temp file first, then an atomic
// rename over the real destination.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// One durable record of a mission's own pending remote-close intent -
/// exactly the two facts `reconcile_pending_remote_closes()` needs to
/// retry it without any help from `MissionRegistry`: which mission, and
/// which terminal outcome (`success`) to report to Job-Dispatcher.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingClose {
    pub mission_id: String,
    pub success: bool,
}

/// One entry as the file stores it: the close plus when it became pending (unix
/// milliseconds; absent in a file written by an older version, which then counts
/// from the moment it was loaded).
#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    mission_id: String,
    success: bool,
    #[serde(default)]
    since_ms: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedOutbox {
    pending: Vec<PersistedEntry>,
}

/// What the outbox remembers about one pending close.
#[derive(Debug, Clone, Copy)]
struct Entry {
    success: bool,
    since_ms: u64,
}

/// The real, shared, durable outbox - one instance lives for this
/// process's whole lifetime (see server.rs's own AppState), guarded by
/// a Mutex the same way MissionRegistry already is, since both the HTTP
/// handlers and the background reconciliation thread touch it.
pub struct RemoteCloseOutbox {
    path: PathBuf,
    pending: Mutex<BTreeMap<String, Entry>>,
}

impl RemoteCloseOutbox {
    /// Loads whatever this process (or an earlier incarnation of it)
    /// already persisted at `path` - a missing file means nothing is
    /// pending yet, not an error. Every other I/O or parse failure is
    /// real and propagated: silently discarding a real outbox that
    /// failed to load would be worse than refusing to start, since it
    /// could mean quietly abandoning a real pending reservation.
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(RemoteCloseOutbox {
                    path,
                    pending: Mutex::new(BTreeMap::new()),
                });
            }
            Err(e) => return Err(e),
        };
        let state: PersistedOutbox = serde_json::from_str(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let loaded_at = crate::logging::now_ms();
        let pending = state
            .pending
            .into_iter()
            .map(|p| {
                let since_ms = if p.since_ms == 0 {
                    loaded_at
                } else {
                    p.since_ms
                };
                (
                    p.mission_id,
                    Entry {
                        success: p.success,
                        since_ms,
                    },
                )
            })
            .collect();
        Ok(RemoteCloseOutbox {
            path,
            pending: Mutex::new(pending),
        })
    }

    fn persist(&self, pending: &BTreeMap<String, Entry>) -> io::Result<()> {
        let state = PersistedOutbox {
            pending: pending
                .iter()
                .map(|(mission_id, entry)| PersistedEntry {
                    mission_id: mission_id.clone(),
                    success: entry.success,
                    since_ms: entry.since_ms,
                })
                .collect(),
        };
        let json = serde_json::to_string_pretty(&state).map_err(io::Error::other)?;
        let tmp_path = self.path.with_extension(format!(
            "{}.{}.tmp",
            self.path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("json"),
            std::process::id()
        ));
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::mission::durable_replace(&tmp_path, &self.path, json.as_bytes())
    }

    /// Records `mission_id` as having a real, still-unconfirmed terminal
    /// outcome - called the moment a confirmation attempt to
    /// Job-Dispatcher fails, same trigger as
    /// `Mission::mark_remote_close_pending`. Best-effort on the persist
    /// itself (logged, never panics): a disk write failing here must not
    /// take down the request that triggered it - the in-memory copy
    /// still drives THIS process's own retry loop either way, the same
    /// honest degradation this ecosystem's other soft-persist call sites
    /// already accept (see e.g. HYDRA-UMC-JOB-DISPATCHER's own
    /// `persistJobLocked`).
    pub fn mark_pending(&self, mission_id: &str, success: bool) {
        let mut pending = self.pending.lock().unwrap();
        // a close that was already pending keeps the moment it first became pending
        let since_ms = pending
            .get(mission_id)
            .map_or_else(crate::logging::now_ms, |e| e.since_ms);
        pending.insert(mission_id.to_string(), Entry { success, since_ms });
        if let Err(e) = self.persist(&pending) {
            crate::log_line!("could not persist pending remote-close outbox: {e}");
        }
    }

    /// Clears `mission_id`'s own pending intent - called once a retried
    /// confirmation actually succeeds.
    pub fn mark_confirmed(&self, mission_id: &str) {
        let mut pending = self.pending.lock().unwrap();
        if pending.remove(mission_id).is_some() {
            if let Err(e) = self.persist(&pending) {
                crate::log_line!("could not persist pending remote-close outbox: {e}");
            }
        }
    }

    /// A real, deterministic snapshot of every mission still pending a
    /// remote-close confirmation - the exact worklist
    /// `reconcile_pending_remote_closes()` walks, independent of whatever
    /// `MissionRegistry` does or does not still know about each one (the
    /// real fix: after a restart, the registry is empty, but this
    /// snapshot still lists it).
    pub fn pending(&self) -> Vec<PendingClose> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .map(|(mission_id, entry)| PendingClose {
                mission_id: mission_id.clone(),
                success: entry.success,
            })
            .collect()
    }

    /// When the close that has waited longest became pending (unix milliseconds), or
    /// None when nothing is pending.
    pub fn oldest_pending_since_ms(&self) -> Option<u64> {
        self.pending
            .lock()
            .unwrap()
            .values()
            .map(|e| e.since_ms)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "orchestrator-outbox-test-{}-{name}.json",
            std::process::id()
        ))
    }

    #[test]
    fn load_of_a_missing_file_is_a_real_empty_outbox_not_an_error() {
        let path = temp_path("missing");
        let outbox = RemoteCloseOutbox::load(path)
            .expect("a missing outbox file must load as empty, not error");
        assert!(outbox.pending().is_empty());
    }

    #[test]
    fn mark_pending_then_reload_survives_a_real_restart() {
        let path = temp_path("survive-restart");
        {
            let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
            outbox.mark_pending("m1", true);
            outbox.mark_pending("m2", false);
        } // `outbox` dropped here - simulates the process exiting
        let reloaded = RemoteCloseOutbox::load(path.clone()).expect("reload must succeed");
        let mut pending = reloaded.pending();
        pending.sort_by(|a, b| a.mission_id.cmp(&b.mission_id));
        assert_eq!(
            pending,
            vec![
                PendingClose {
                    mission_id: "m1".into(),
                    success: true
                },
                PendingClose {
                    mission_id: "m2".into(),
                    success: false
                },
            ]
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn mark_confirmed_removes_the_entry_and_persists_it() {
        let path = temp_path("confirmed");
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        outbox.mark_pending("m1", true);
        outbox.mark_confirmed("m1");
        assert!(outbox.pending().is_empty());

        let reloaded = RemoteCloseOutbox::load(path.clone()).expect("reload must succeed");
        assert!(
            reloaded.pending().is_empty(),
            "the confirmation must have been persisted, not just applied in memory"
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn mark_confirmed_on_an_unknown_mission_is_a_safe_no_op() {
        let path = temp_path("unknown-confirm");
        let outbox = RemoteCloseOutbox::load(path).unwrap();
        outbox.mark_confirmed("does-not-exist"); // must not panic
        assert!(outbox.pending().is_empty());
    }

    #[test]
    fn a_second_mark_pending_for_the_same_mission_overwrites_not_duplicates() {
        let path = temp_path("overwrite");
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        outbox.mark_pending("m1", true);
        outbox.mark_pending("m1", false); // a later local re-decision, e.g. cancel after complete's own failed confirmation was somehow retried
        let pending = outbox.pending();
        assert_eq!(
            pending,
            vec![PendingClose {
                mission_id: "m1".into(),
                success: false
            }]
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file_behind() {
        let path = temp_path("atomic");
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        outbox.mark_pending("m1", true);
        let dir = path.parent().unwrap();
        let stem = path.file_name().unwrap().to_string_lossy().to_string();
        let leftover: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with(&stem) && name != stem
            })
            .collect();
        assert!(
            leftover.is_empty(),
            "expected no leftover temp file, found: {leftover:?}"
        );
        fs::remove_file(&path).ok();
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "orchestrator-outbox-edge-{}-{name}.json",
            std::process::id()
        ));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn a_corrupt_outbox_is_refused_because_it_may_hold_a_real_pending_close() {
        for (name, content) in [
            ("garbage", "garbage"),
            ("blank", ""),
            ("wrong-shape", "[1,2,3]"),
        ] {
            let path = temp_file(name, content);
            let err = RemoteCloseOutbox::load(path.clone())
                .map(|_| ())
                .expect_err("must not load");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{name}");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                content,
                "the file is left for inspection"
            );
            fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn pending_is_sorted_and_keeps_the_success_flag_of_each_mission() {
        let path = std::env::temp_dir().join(format!(
            "orchestrator-outbox-edge-{}-order.json",
            std::process::id()
        ));
        fs::remove_file(&path).ok();
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        outbox.mark_pending("b", false);
        outbox.mark_pending("a", true);
        outbox.mark_pending("c", false);
        let pending = outbox.pending();
        let ids: Vec<&str> = pending.iter().map(|p| p.mission_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert!(pending[0].success && !pending[1].success && !pending[2].success);
        // A reload sees exactly the same worklist.
        let reloaded = RemoteCloseOutbox::load(path.clone()).unwrap();
        assert_eq!(reloaded.pending().len(), 3);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn confirming_the_last_entry_leaves_an_empty_persisted_outbox() {
        let path = std::env::temp_dir().join(format!(
            "orchestrator-outbox-edge-{}-emptied.json",
            std::process::id()
        ));
        fs::remove_file(&path).ok();
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        outbox.mark_pending("m1", true);
        outbox.mark_confirmed("m1");
        assert!(RemoteCloseOutbox::load(path.clone())
            .unwrap()
            .pending()
            .is_empty());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_pending_close_is_stored_even_when_the_parent_directories_do_not_exist() {
        let dir = std::env::temp_dir().join(format!(
            "orchestrator-outbox-edge-{}-nested",
            std::process::id()
        ));
        let path = dir.join("a").join("b").join("outbox.json");
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        outbox.mark_pending("m1", true);
        assert!(path.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod age_tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("orc-outbox-age-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn nothing_pending_has_no_age() {
        let path = temp("none");
        fs::remove_file(&path).ok();
        let outbox = RemoteCloseOutbox::load(path).unwrap();
        assert_eq!(outbox.oldest_pending_since_ms(), None);
    }

    #[test]
    fn the_oldest_close_keeps_the_moment_it_first_became_pending() {
        let path = temp("oldest");
        fs::write(
            &path,
            r#"{"pending":[{"mission_id":"a","success":true,"since_ms":1000},{"mission_id":"b","success":false,"since_ms":5000}]}"#,
        )
        .unwrap();
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        assert_eq!(outbox.oldest_pending_since_ms(), Some(1000));
        outbox.mark_pending("a", false); // a later decision for the same mission
        assert_eq!(outbox.oldest_pending_since_ms(), Some(1000));
        outbox.mark_confirmed("a");
        assert_eq!(outbox.oldest_pending_since_ms(), Some(5000));
        let reloaded = RemoteCloseOutbox::load(path.clone()).unwrap();
        assert_eq!(reloaded.oldest_pending_since_ms(), Some(5000));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_from_before_the_timestamps_counts_from_the_load() {
        let path = temp("legacy");
        fs::write(&path, r#"{"pending":[{"mission_id":"a","success":true}]}"#).unwrap();
        let before = crate::logging::now_ms();
        let outbox = RemoteCloseOutbox::load(path.clone()).unwrap();
        let since = outbox.oldest_pending_since_ms().unwrap();
        assert!(since >= before, "{since} < {before}");
        assert_eq!(outbox.pending().len(), 1);
        fs::remove_file(&path).ok();
    }
}
