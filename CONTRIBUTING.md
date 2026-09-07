# Contributing to HYDRA-UMC-ORCHESTRATOR 🦾

We welcome contributions to the distributed swarm manager of the HYDRA-UMC platform.

## Technology Stack
- **Languages**: Rust 1.80+, Go 1.22+.
- **Communication**: gRPC, Protocol Buffers, PTP (IEEE 1588).
- **Architecture**: Distributed Edge, Event-Driven.
- **Infrastructure**: Linux (Ubuntu 22.04).

## Guidelines
1. **Concurrency Safety**: Use Rust's ownership model and Go's channels to ensure thread-safe swarm orchestration.
2. **Network Resilience**: All inter-node communications must handle packet loss and high latency gracefully.
3. **Safety First**: Any changes to the global E-STOP or health monitoring logic must be Peer Reviewed by two senior developers.
4. **Protobuf Consistency**: Ensure all `.proto` changes are backward compatible with the current HydraNode versions.
