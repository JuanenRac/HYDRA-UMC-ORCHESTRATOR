# proto/ — Shared gRPC contract for node-to-node traffic

This folder holds `.proto` schema files consumed by every node across the
ecosystem's Vision AI Node, Cognitive AI Node, Orchestration & Swarm, and
Digital Twin & Simulation families - Python, Rust, Go, and Node.js code
alike generates its own language bindings from the *same* source files
here, so a schema change is one edit instead of N independently-drifting
copies.

It lives inside this repo (rather than a dedicated `HYDRA-UMC-PROTO` repo)
because `HYDRA-UMC-ORCHESTRATOR` is already the integration parent with
authority over the whole fleet - this avoids creating a whole separate
repository for a single shared file until there's a real need to
version/release it independently. Moving it later is a straightforward
copy, not a rewrite.

## Files

- **`hydra_common.proto`** — the one contract every node is meant to implement:
  `NodeIdentity`, `HealthReport`, and `HealthService` with its two calls:
  `Check()` (one report) and `Watch()` (the same report as a server stream).
  HYDRA-UMC-ORCHESTRATOR and HYDRA-UMC-JOB-DISPATCHER serve it when their
  `health_grpc` section is switched on (off by default, loopback by default);
  the other nodes are still probed over their HTTP health route. The states
  read like the standard gRPC health protocol: `OK` is SERVING, `DEGRADED` is
  SERVING with a concern, `UNHEALTHY` is NOT_SERVING and `UNSPECIFIED` is
  UNKNOWN. It is what lets [HYDRA-UMC-NODE-HEALING](https://github.com/JuanenRac/HYDRA-UMC-NODE-HEALING)
  probe any node in the ecosystem the same way, instead of each family
  inventing its own ad-hoc health check.

Per-family business services (vision detections, cognitive intents, job
dispatch, physics stepping, ...) are intentionally **not** defined yet -
each gets its own `.proto` file once that node's real logic lands, so the
message shape is designed against an actual implementation instead of
guessed in advance.

## Generating language bindings

This repo does not commit generated code - each consuming project
generates its own bindings as part of its own build step, from this same
source. Examples:

```bash
# Python (pip install grpcio-tools)
python -m grpc_tools.protoc -I proto \
  --python_out=OUT_DIR --grpc_python_out=OUT_DIR \
  proto/hydra_common.proto

# Rust (tonic-prost-build, from a build.rs: this repo does it, with the protoc
# of the protoc-bin-vendored crate, so no tool has to be installed)
tonic_prost_build::compile_protos("proto/hydra_common.proto")?;

# Go (protoc-gen-go + protoc-gen-go-grpc)
protoc -I proto --go_out=. --go-grpc_out=. proto/hydra_common.proto

# Node/TypeScript (ts-proto or grpc-tools)
protoc -I proto --plugin=protoc-gen-ts_proto \
  --ts_proto_out=OUT_DIR proto/hydra_common.proto
```

Verified for real (not just written): `hydra_common.proto` was compiled
with `grpc_tools.protoc` (Python target), and the generated stub was used
to build a real `HealthReport` message, serialize it, and parse it back
byte-for-byte - the schema is genuinely valid protobuf3, not just
plausible-looking text.

## Versioning

No version bump here yet - this is schema, not a runnable
binary. `package hydra.common.v1` inside the `.proto` file itself is the
version marker: a breaking change bumps to `hydra.common.v2` in a new
package/file rather than silently changing `v1` under every consumer's
feet.
