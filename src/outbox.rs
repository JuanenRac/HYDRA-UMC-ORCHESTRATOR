// HYDRA-UMC-ORCHESTRATOR - src/outbox.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0 - see LICENSE
//
// V07-012 (found in an independent revalidation audit, P1): server.rs's
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
// This is the real, durable "outbox" the audit's own proposed fix names:
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

#[derive(Serialize, Deserialize, Default)]
struct PersistedOutbox {
    pending: Vec<PendingClose>,
}

/// The real, shared, durable outbox - one instance lives for this
/// process's whole lifetime (see server.rs's own AppState), guarded by
/// a Mutex the same way MissionRegistry already is, since both the HTTP
/// handlers and the background reconciliation thread touch it.
pub struct RemoteCloseOutbox {
    path: PathBuf,
    pending: Mutex<BTreeMap<String, bool>>,
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
        let pending = state
            .pending
            .into_iter()
            .map(|p| (p.mission_id, p.success))
            .collect();
        Ok(RemoteCloseOutbox {
            path,
            pending: Mutex::new(pending),
        })
    }

    fn persist(&self, pending: &BTreeMap<String, bool>) -> io::Result<()> {
        let state = PersistedOutbox {
            pending: pending
                .iter()
                .map(|(mission_id, success)| PendingClose {
                    mission_id: mission_id.clone(),
                    success: *success,
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
        fs::write(&tmp_path, json)?;
        fs::rename(&tmp_path, &self.path)
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
        pending.insert(mission_id.to_string(), success);
        if let Err(e) = self.persist(&pending) {
            eprintln!("[orchestrator] could not persist pending remote-close outbox: {e}");
        }
    }

    /// Clears `mission_id`'s own pending intent - called once a retried
    /// confirmation actually succeeds.
    pub fn mark_confirmed(&self, mission_id: &str) {
        let mut pending = self.pending.lock().unwrap();
        if pending.remove(mission_id).is_some() {
            if let Err(e) = self.persist(&pending) {
                eprintln!("[orchestrator] could not persist pending remote-close outbox: {e}");
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
            .map(|(mission_id, success)| PendingClose {
                mission_id: mission_id.clone(),
                success: *success,
            })
            .collect()
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
