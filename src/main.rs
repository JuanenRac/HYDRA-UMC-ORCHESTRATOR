// HYDRA-UMC-ORCHESTRATOR - entry point
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0 - see LICENSE
//
// Why Rust for this specific service: this process is the one with the
// most authority over the fleet - it is what sends the global E-STOP and
// arbitrates which robot gets which mission. That role needs deterministic,
// low-latency coordination (no garbage collector pauses that could delay a
// safety-critical stop) and compile-time memory/type safety (a crash or a
// data race here does not stay local - it can leave the whole swarm without
// its coordinator mid-mission). Go, used by two of this orchestrator's own
// children (JOB-DISPATCHER, NODE-HEALING), is a fine fit for their simpler,
// more isolated jobs; it is not the right trade-off for the "brain" itself.
//
// Bare invocation stays a minimal skeleton: prints identity and exits 0.
// The distributed swarm coordination logic (mission queue integration,
// PTP-synced dispatch, fleet-wide health aggregation) is built out
// incrementally on top of this entry point. Real logic lands as pure,
// no-I/O modules first (see mission.rs) - the same "real logic before
// real transport" sequencing used across this ecosystem's other v0
// passes - wired to a real gRPC/network layer only once there is a real
// peer (JOB-DISPATCHER, NODE-HEALING) on the other end to talk to.
//
// `mission-demo` runs the real mission.rs state machine end-to-end
// (dispatch -> in-progress -> node failure -> recovery -> idempotent
// cancel -> completion) against an in-memory MissionRegistry and prints
// every real transition, so the logic is exercisable without a server.

mod mission;

use mission::{CancelOutcome, MissionRegistry, MissionState};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ROLE: &str = "Distributed swarm manager: coordinates SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER and NODE-HEALING as a single unified robot fleet.";

fn state_label(state: &MissionState) -> String {
    match state {
        MissionState::Pending => "Pending".to_string(),
        MissionState::Dispatched { node } => format!("Dispatched(node={node})"),
        MissionState::InProgress { node } => format!("InProgress(node={node})"),
        MissionState::Completed { node } => format!("Completed(node={node})"),
        MissionState::Cancelled => "Cancelled".to_string(),
        MissionState::Failed { reason } => format!("Failed({reason})"),
    }
}

fn run_mission_demo() {
    let mut registry = MissionRegistry::new();

    registry.add("mission-1").dispatch("node-a").unwrap();
    registry.add("mission-2").dispatch("node-a").unwrap();
    registry.add("mission-3").dispatch("node-b").unwrap();
    registry.get_mut("mission-1").unwrap().start().unwrap();
    registry.get_mut("mission-3").unwrap().start().unwrap();
    for id in ["mission-1", "mission-2", "mission-3"] {
        println!(
            "[orchestrator] {id}: dispatched -> {}",
            state_label(&registry.get(id).unwrap().state)
        );
    }

    println!("[orchestrator] node-a reported UNREACHABLE by NODE-HEALING - recovering its missions");
    let requeued = registry.recover_node_unavailable("node-a");
    for id in &requeued {
        println!(
            "[orchestrator] {id}: requeued -> {}",
            state_label(&registry.get(id).unwrap().state)
        );
    }
    println!(
        "[orchestrator] mission-3: unaffected (different node) -> {}",
        state_label(&registry.get("mission-3").unwrap().state)
    );

    let outcome = registry.get_mut("mission-2").unwrap().cancel().unwrap();
    println!("[orchestrator] mission-2: cancel() -> {outcome:?} -> {}", state_label(&registry.get("mission-2").unwrap().state));
    let outcome = registry.get_mut("mission-2").unwrap().cancel().unwrap();
    println!("[orchestrator] mission-2: cancel() again (idempotent) -> {outcome:?} -> {}", state_label(&registry.get("mission-2").unwrap().state));
    assert_eq!(outcome, CancelOutcome::AlreadyCancelled);

    registry.get_mut("mission-3").unwrap().complete().unwrap();
    println!(
        "[orchestrator] mission-3: complete() -> {}",
        state_label(&registry.get("mission-3").unwrap().state)
    );

    registry.add("mission-4").dispatch("node-c").unwrap();
    registry
        .get_mut("mission-4")
        .unwrap()
        .fail("no healthy node accepted redispatch after 3 attempts")
        .unwrap();
    println!(
        "[orchestrator] mission-4: fail() -> {}",
        state_label(&registry.get("mission-4").unwrap().state)
    );

    println!("[orchestrator] final registry state:");
    for m in registry.all() {
        println!(
            "  {}: {} (terminal={})",
            m.id,
            state_label(&m.state),
            m.is_terminal()
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("mission-demo") => run_mission_demo(),
        _ => {
            println!("HYDRA-UMC-ORCHESTRATOR v{VERSION}");
            println!("{ROLE}");
        }
    }
}
