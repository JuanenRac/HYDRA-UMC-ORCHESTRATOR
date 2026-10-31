// HYDRA-UMC-ORCHESTRATOR - tests/cli_modes.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
//
// What a process supervisor and an operator see: the identity banner, the
// mission demo transcript, and the fatal start-up refusals of `serve`
// (exit status 1, nothing announced as listening). None of these starts a
// server.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn run_in(dir: &PathBuf, args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_hydra-umc-orchestrator"))
        .args(args)
        .current_dir(dir)
        .env_remove("ORCHESTRATOR_TOKEN")
        .env_remove("ORCHESTRATOR_LOG_FORMAT")
        .env_remove("JOB_DISPATCHER_TOKEN")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("orchestrator-modes-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn no_arguments_or_an_unknown_command_print_the_identity() {
    let dir = scratch("identity");
    for args in [vec![], vec!["nonsense"]] {
        let (code, stdout, _) = run_in(&dir, &args);
        assert_eq!(code, Some(0));
        assert!(stdout.starts_with("HYDRA-UMC-ORCHESTRATOR v"), "{stdout}");
        assert!(stdout.contains("Distributed swarm manager"), "{stdout}");
    }
}

#[test]
fn the_mission_demo_walks_a_node_failure_and_an_idempotent_cancel() {
    let dir = scratch("demo");
    let (code, stdout, _) = run_in(&dir, &["mission-demo"]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(
        stdout.contains("mission-1: dispatched -> InProgress(node=node-a)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("mission-2: dispatched -> Dispatched(node=node-a)"),
        "{stdout}"
    );
    assert!(stdout.contains("node-a reported UNREACHABLE"), "{stdout}");
    assert!(
        stdout.contains("mission-3: unaffected (different node) -> InProgress(node=node-b)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("cancel() again (idempotent) -> AlreadyCancelled -> Cancelled"),
        "{stdout}"
    );
    assert!(
        stdout.contains("mission-3: complete() -> Completed(node=node-b)"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "mission-4: fail() -> Failed(no healthy node accepted redispatch after 3 attempts)"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("mission-4: Failed(") && stdout.contains("(terminal=true)"),
        "{stdout}"
    );
}

fn assert_fatal(args: &[&str], fragment: &str, dir: &PathBuf) {
    let (code, _, stderr) = run_in(dir, args);
    assert_eq!(code, Some(1), "{args:?}: {stderr}");
    assert!(stderr.contains("fatal:"), "{args:?}: {stderr}");
    assert!(stderr.contains(fragment), "{args:?}: {stderr}");
    assert!(!stderr.contains("HTTP API listening"), "{args:?}: {stderr}");
}

#[test]
fn serve_refuses_bad_options_with_a_nonzero_exit() {
    let dir = scratch("serve");
    assert_fatal(
        &["serve", "--log-format", "xml"],
        "--log-format must be text or json",
        &dir,
    );
    assert_fatal(
        &["serve", "--keep-finished", "many"],
        "--keep-finished must be a whole number",
        &dir,
    );
    assert_fatal(
        &["serve", "--addr", "0.0.0.0", "--port", "0"],
        "refusing to listen on 0.0.0.0:0 without a token",
        &dir,
    );
}

#[test]
fn a_corrupt_outbox_or_mission_snapshot_stops_the_start() {
    let dir = scratch("corrupt-outbox");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(
        dir.join("data").join("pending_remote_closes.json"),
        "not json",
    )
    .unwrap();
    assert_fatal(
        &["serve", "--port", "0"],
        "could not load the pending remote-close outbox",
        &dir,
    );

    let dir = scratch("corrupt-missions");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("data").join("missions.json"), "{broken").unwrap();
    assert_fatal(
        &["serve", "--port", "0"],
        "could not load the mission registry",
        &dir,
    );
}

#[test]
fn json_log_format_is_accepted_and_applies_to_the_fatal_line() {
    let dir = scratch("json-log");
    let (code, _, stderr) = run_in(
        &dir,
        &[
            "serve",
            "--log-format",
            "json",
            "--addr",
            "0.0.0.0",
            "--port",
            "0",
        ],
    );
    assert_eq!(code, Some(1));
    let line = stderr
        .lines()
        .find(|l| l.contains("refusing to listen"))
        .expect("the fatal line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("one JSON object per line");
    assert!(parsed.to_string().contains("refusing to listen"), "{line}");
}
