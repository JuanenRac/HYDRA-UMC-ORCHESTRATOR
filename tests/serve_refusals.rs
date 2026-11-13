// HYDRA-UMC-ORCHESTRATOR - tests/serve_refusals.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
//
// The fatal start-up refusals of `serve` that come before the registry is read and before any port is bound: the identity key, the replication to
// swarm-sync and the lease limit. Each one exits with status 1 and never announces a listener. None of these starts a server.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "orchestrator-refusals-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn serve(dir: &PathBuf, args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_hydra-umc-orchestrator"))
        .arg("serve")
        .args(args)
        .current_dir(dir)
        .env_remove("ORCHESTRATOR_TOKEN")
        .env_remove("ORCHESTRATOR_LOG_FORMAT")
        .env_remove("ORCHESTRATOR_IDENTITY_KEY_FILE")
        .env_remove("ORCHESTRATOR_NAME")
        .env_remove("ORCHESTRATOR_REQUIRE_HEALER_LEASE")
        .env_remove("SWARM_SYNC_TOKEN")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn assert_fatal(args: &[&str], fragment: &str) {
    let dir = scratch();
    let (code, stderr) = serve(&dir, args);
    assert_eq!(code, Some(1), "{args:?}: {stderr}");
    assert!(stderr.contains("fatal:"), "{args:?}: {stderr}");
    assert!(stderr.contains(fragment), "{args:?}: {stderr}");
    assert!(!stderr.contains("HTTP API listening"), "{args:?}: {stderr}");
}

#[test]
fn an_identity_key_file_that_cannot_be_used_stops_the_start() {
    assert_fatal(
        &["--identity-key-file", "no-such-key-file"],
        "--identity-key-file",
    );
}

#[test]
fn the_replication_to_swarm_sync_refuses_an_address_an_interval_and_a_name_it_cannot_use() {
    assert_fatal(
        &["--swarm-sync-url", "ftp://10.0.0.7:8112"],
        "--swarm-sync-url must start with http:// or https://",
    );
    for interval in ["0", "3601", "often"] {
        assert_fatal(
            &[
                "--swarm-sync-url",
                "http://10.0.0.7:8112",
                "--swarm-sync-interval-s",
                interval,
            ],
            "--swarm-sync-interval-s must be a whole number from 1 to 3600",
        );
    }
    for name in ["has space", "a/b", "semi;colon"] {
        assert_fatal(
            &[
                "--swarm-sync-url",
                "http://10.0.0.7:8112",
                "--orchestrator-name",
                name,
            ],
            "--orchestrator-name must be a plain name",
        );
    }
}

#[test]
fn a_lease_limit_under_the_minimum_or_not_a_number_stops_the_start() {
    for value in ["0", "999", "soon"] {
        assert_fatal(
            &["--lease-max-ttl-ms", value],
            "--lease-max-ttl-ms must be a whole number of at least 1000",
        );
    }
}

#[test]
fn the_swarm_sync_settings_are_not_read_when_the_replication_is_off() {
    // an interval and a name without an address are ignored: the refusal comes only with --swarm-sync-url
    let dir = scratch();
    let (code, stderr) = serve(
        &dir,
        &[
            "--port",
            "0",
            "--swarm-sync-interval-s",
            "0",
            "--log-format",
            "xml",
        ],
    );
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("--log-format must be text or json"),
        "the first refusal is the one that applies: {stderr}"
    );
    assert!(!stderr.contains("--swarm-sync-interval-s"), "{stderr}");
}
