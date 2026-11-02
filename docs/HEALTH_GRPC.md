<!-- =============================================================================
HYDRA-UMC-ORCHESTRATOR - Health service over gRPC
Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
GPL-3.0-or-later - see LICENSE
============================================================================= -->

# Health service over gRPC

The orchestrator can serve the `HealthService` of the shared contract (`proto/hydra_common.proto`, package `hydra.common.v1`) so that HYDRA-UMC-NODE-HEALING asks `Check()` and follows `Watch()` instead of polling `GET /health`. It is **off by default**: nothing listens until the configuration file turns it on. It only reads; it changes no mission, sends no stop and has no say in the physical stop.

```json
{
  "health_grpc": {
    "enabled": true,
    "listen": "127.0.0.1:50114",
    "token_env": "ORCHESTRATOR_HEALTH_TOKEN",
    "max_connections": 16,
    "max_watchers": 8,
    "watch_interval_seconds": 5,
    "keepalive_seconds": 30
  }
}
```

Every key but `enabled` is optional and an unknown key stops the start (see the configuration file in [`CLI_REFERENCE.md`](CLI_REFERENCE.md)).

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Without `true` the section does nothing, whatever else it says. |
| `listen` | `127.0.0.1:50114` | An IP address and a port. Loopback unless you say otherwise. |
| `token_env` | none | Environment variable that holds the bearer token. On an address other than loopback the token of the HTTP API (`--token`) is used when this is not named, and without any token the service refuses to start. |
| `max_connections` | 16 | Connections open at once (1 to 1024). One past the limit is closed at once. |
| `max_watchers` | 8 | `Watch()` streams open at once (1 to 1024). One more is answered `RESOURCE_EXHAUSTED`. |
| `watch_interval_seconds` | 5 | A stream repeats the report at least this often when nothing changed (0.1 to 60). It is the keepalive of the stream. |
| `keepalive_seconds` | 30 | Period of the HTTP/2 pings; a client that does not answer within the same time is dropped (1 to 3600). |

## What it says

`Check()` returns one `HealthReport`; `Watch()` returns the same report as a stream: the current one at once, another whenever the state or the detail changes, and a repeat every `watch_interval_seconds`, until the client cancels or the service stops.

| `HealthState` | Read like `grpc.health.v1` | When |
|---|---|---|
| `HEALTH_STATE_OK` | `SERVING` | The mission registry can be read and the global stop is not engaged. |
| `HEALTH_STATE_DEGRADED` | `SERVING` with a concern | The global stop is engaged: the service answers, but no mission moves until it is released. |
| `HEALTH_STATE_UNHEALTHY` | `NOT_SERVING` | The mission registry is not usable (a worker panicked while holding it). |
| `HEALTH_STATE_UNSPECIFIED` | `UNKNOWN` | Never sent by this service. |

A registry that is busy for a moment is not a failure and the answer never waits for it. The identity is `HYDRA-UMC-ORCHESTRATOR` with the version of the binary, and the metrics carry `missions`, `missions_active`, `missions_unknown` and `global_stop` (numbers and a flag, never a mission, a node or a token).

## Pointing the watchdog at it

HYDRA-UMC-NODE-HEALING takes a registry entry with an `address` (gRPC) instead of a `healthUrl`. The `name` has to be exactly the name the service answers with, or the watchdog treats the node as invalid:

```json
{ "name": "HYDRA-UMC-ORCHESTRATOR", "address": "127.0.0.1:50114", "watch": true }
```

`"watch": true` makes the watchdog open `Watch()`; a service that does not serve it is polled with `Check()` as before.

The watchdog dials without credentials and without TLS today, so it can only reach a service that is on this machine and has no token (the default). A token or a network address works for any other client that sends `authorization: Bearer <token>`, but not yet for the watchdog.

## Limits and what is not covered

* Messages from the client are limited to 1 KiB, the number of open streams per connection is bounded, and every task of a stream ends when its client goes away or the service stops.
* There is no TLS: the transport is plain HTTP/2, so use a loopback address or a network you trust.
* It has been tested only against a gRPC client of its own in the tests, never against a live HYDRA-UMC-NODE-HEALING, a robot or a cell.
