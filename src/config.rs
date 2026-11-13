// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/config.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The optional configuration file (`--config`, or `ORCHESTRATOR_CONFIG`): the tuned values of
//! the global stop and of the events of Job-Dispatcher. Secrets are never in the file: a section
//! names the environment variable that holds each one. An unknown field is an error, so a
//! misspelt switch cannot silently stay off.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::estop::EstopConfig;
use crate::health_grpc::HealthGrpcConfig;
use crate::job_events::JobEventsConfig;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub estop: Option<EstopSection>,
    pub job_events: Option<JobEventsSection>,
    pub health_grpc: Option<HealthGrpcSection>,
    pub metrics: Option<MetricsSection>,
    /// The watched node (the name NODE-HEALING dials, usually a service) -> the robot id the missions were dispatched with. Without it a node is its own robot.
    pub node_robots: Option<std::collections::BTreeMap<String, String>>,
}

/// The "metrics" section: `GET /metrics` in the text format of Prometheus. Off unless enabled (closed, the route answers `404` like an unknown
/// one). Open, it answers a caller on this machine, or any caller that presents the bearer token when the API has one.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    #[serde(default)]
    pub enabled: bool,
}

/// Whether the configuration opens `GET /metrics` (off by default).
pub fn metrics_enabled(file: &FileConfig) -> bool {
    file.metrics.as_ref().is_some_and(|s| s.enabled)
}

/// The longest name of a node or a robot the table accepts.
const MAX_NAME_CHARS: usize = 128;

/// The table `node_robots` of the file, checked: every name is non-empty, at most 128 characters and free of control characters. A robot may have several nodes,
/// a node has one robot. Empty when the file has none (a node is then its own robot, as it always was).
pub fn node_robots(
    file: &FileConfig,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let Some(table) = &file.node_robots else {
        return Ok(Default::default());
    };
    for (node, robot) in table {
        for (what, name) in [("node", node), ("robot", robot)] {
            if name.trim().is_empty()
                || name.chars().count() > MAX_NAME_CHARS
                || name.chars().any(char::is_control)
            {
                return Err(format!(
                    "node_robots: a {what} name must be 1 to {MAX_NAME_CHARS} characters without control characters, not {name:?}"
                ));
            }
        }
    }
    Ok(table.clone())
}

/// The robot a watched node stands for: the one the table says, else the node itself.
pub fn robot_for_node<'a>(
    table: &'a std::collections::BTreeMap<String, String>,
    node: &'a str,
) -> &'a str {
    table.get(node).map_or(node, String::as_str)
}

/// The "estop" section. The stop itself is always available; this only says where it is forwarded and who may release it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstopSection {
    /// base URL of HYDRA-UMC-SERVER; the stop is forwarded to its POST /api/stop-all
    pub server_url: Option<String>,
    pub server_token_env: Option<String>,
    pub fanout_timeout_seconds: Option<f64>,
    /// environment variable with the token of the administrator, the only one that may release the stop
    pub admin_token_env: Option<String>,
}

/// The "job_events" section: off unless enabled.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobEventsSection {
    #[serde(default)]
    pub enabled: bool,
    /// environment variable with the secret shared with Job-Dispatcher
    pub secret_env: Option<String>,
    pub max_skew_seconds: Option<u64>,
}

/// The "health_grpc" section: the health service of the shared contract over gRPC. Off unless enabled; loopback unless an
/// address is given, and an address other than loopback needs a token.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthGrpcSection {
    #[serde(default)]
    pub enabled: bool,
    /// address to listen on, an IP and a port (default 127.0.0.1:50114)
    pub listen: Option<String>,
    /// environment variable with the bearer token; when absent, an address other than loopback uses the token of the HTTP API (if any)
    pub token_env: Option<String>,
    pub max_connections: Option<usize>,
    pub max_watchers: Option<usize>,
    pub watch_interval_seconds: Option<f64>,
    pub keepalive_seconds: Option<f64>,
}

pub fn parse(text: &str) -> Result<FileConfig, String> {
    if text.trim().is_empty() {
        return Ok(FileConfig::default());
    }
    serde_json::from_str(text).map_err(|e| format!("configuration: {e}"))
}

pub fn load(path: &str) -> Result<FileConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("configuration {path}: {e}"))?;
    parse(&text)
}

fn env_value(name: &Option<String>) -> Result<Option<String>, String> {
    match name {
        None => Ok(None),
        Some(var) => match std::env::var(var) {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            _ => Err(format!(
                "the environment variable {var} named in the configuration is empty or not set"
            )),
        },
    }
}

/// Builds the settings the server runs with from the file and the data directory.
pub fn build(
    file: &FileConfig,
    data_dir: &str,
) -> Result<(EstopConfig, Option<JobEventsConfig>), String> {
    let mut estop = EstopConfig {
        state_file: Some(PathBuf::from(data_dir).join("estop.json")),
        audit_file: Some(PathBuf::from(data_dir).join("estop_audit.jsonl")),
        ..EstopConfig::default()
    };
    if let Some(section) = &file.estop {
        if let Some(url) = &section.server_url {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err("estop.server_url must start with http:// or https://".into());
            }
            estop.server_url = Some(url.clone());
        }
        estop.server_token = env_value(&section.server_token_env)?;
        estop.admin_token = env_value(&section.admin_token_env)?;
        if let Some(seconds) = section.fanout_timeout_seconds {
            if !(seconds > 0.0 && seconds <= 60.0) {
                return Err(
                    "estop.fanout_timeout_seconds must be greater than 0 and at most 60".into(),
                );
            }
            estop.fanout_timeout = Duration::from_secs_f64(seconds);
        }
    }
    let job_events = match &file.job_events {
        Some(section) if section.enabled => {
            let secret = env_value(&section.secret_env)?
                .ok_or("job_events.secret_env is required when job_events is enabled")?;
            let skew = section.max_skew_seconds.unwrap_or(300);
            if skew == 0 || skew > 86_400 {
                return Err("job_events.max_skew_seconds must be between 1 and 86400".into());
            }
            Some(JobEventsConfig {
                secret: secret.into_bytes(),
                max_skew_seconds: skew,
            })
        }
        _ => None,
    };
    Ok((estop, job_events))
}

/// The settings of the gRPC health service, or `None` while it is off. `api_token` is the token of the HTTP API: it is the
/// fallback when `token_env` is not named, and only for an address other than loopback. An address other than loopback without any token (or the identity key
/// of the server, `identity_key`) is refused.
pub fn health_grpc(
    file: &FileConfig,
    api_token: Option<&str>,
    identity_key: Option<&[u8]>,
) -> Result<Option<HealthGrpcConfig>, String> {
    let Some(section) = file.health_grpc.as_ref().filter(|s| s.enabled) else {
        return Ok(None);
    };
    let mut cfg = HealthGrpcConfig::default();
    if let Some(listen) = &section.listen {
        cfg.listen = listen.parse().map_err(|_| {
            format!("health_grpc.listen must be an IP address and a port such as 127.0.0.1:50114, not {listen:?}")
        })?;
    }
    // The token of the HTTP API is a fallback only for an address reachable from the network: on loopback, with no token named, the
    // service is open to this machine (the watchdog of the cell dials it without credentials).
    cfg.token = match env_value(&section.token_env)? {
        Some(token) => Some(token),
        None if !cfg.listen.ip().is_loopback() => {
            api_token.filter(|t| !t.is_empty()).map(str::to_string)
        }
        None => None,
    };
    // with the identity key of the family the signed token of the server is accepted too and nothing is open: an address reachable from the network needs no token of its own
    cfg.identity_key = identity_key.map(<[u8]>::to_vec);
    if cfg.token.is_none() && cfg.identity_key.is_none() && !cfg.listen.ip().is_loopback() {
        return Err("health_grpc.listen is reachable from the network but there is no token: name one with health_grpc.token_env (or run the API with a token), or listen on 127.0.0.1".into());
    }
    if let Some(n) = section.max_connections {
        if !(1..=1024).contains(&n) {
            return Err("health_grpc.max_connections must be between 1 and 1024".into());
        }
        cfg.max_connections = n;
    }
    if let Some(n) = section.max_watchers {
        if !(1..=1024).contains(&n) {
            return Err("health_grpc.max_watchers must be between 1 and 1024".into());
        }
        cfg.max_watchers = n;
    }
    if let Some(s) = section.watch_interval_seconds {
        if !(0.1..=60.0).contains(&s) {
            return Err("health_grpc.watch_interval_seconds must be between 0.1 and 60".into());
        }
        cfg.watch_interval = Duration::from_secs_f64(s);
    }
    if let Some(s) = section.keepalive_seconds {
        if !(1.0..=3600.0).contains(&s) {
            return Err("health_grpc.keepalive_seconds must be between 1 and 3600".into());
        }
        cfg.keepalive = Duration::from_secs_f64(s);
    }
    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_node_table_joins_a_watched_service_with_its_robot() {
        let file = parse(r#"{"node_robots": {"hydra-umc-bridge-cnc": "arm-1", "hydra-umc-bridge-laser": "arm-1"}}"#).unwrap();
        let table = node_robots(&file).unwrap();
        assert_eq!(robot_for_node(&table, "hydra-umc-bridge-cnc"), "arm-1");
        assert_eq!(robot_for_node(&table, "hydra-umc-bridge-laser"), "arm-1");
        assert_eq!(robot_for_node(&table, "arm-2"), "arm-2");
        assert!(node_robots(&parse("{}").unwrap()).unwrap().is_empty());
    }

    #[test]
    fn a_bad_name_in_the_node_table_stops_the_start() {
        for text in [
            r#"{"node_robots": {"": "arm-1"}}"#,
            r#"{"node_robots": {"svc": "  "}}"#,
            r#"{"node_robots": {"svc": "arm\u0001"}}"#,
        ] {
            let file = parse(text).unwrap();
            assert!(
                node_robots(&file).unwrap_err().contains("node_robots"),
                "{text}"
            );
        }
        let long = format!(r#"{{"node_robots": {{"svc": "{}"}}}}"#, "r".repeat(129));
        assert!(node_robots(&parse(&long).unwrap()).is_err());
        assert!(
            parse(r#"{"node_robot": {}}"#).is_err(),
            "a misspelt key is an error"
        );
    }

    #[test]
    fn empty_and_missing_sections_change_nothing() {
        let file = parse("").unwrap();
        let (estop, events) = build(&file, "data").unwrap();
        assert!(events.is_none());
        assert!(estop.server_url.is_none() && estop.admin_token.is_none());
        assert!(estop.state_file.is_some(), "the latch is saved by default");
        assert!(build(&parse("{}").unwrap(), "data").unwrap().1.is_none());
    }

    #[test]
    fn a_misspelt_field_is_an_error() {
        assert!(parse(r#"{"estopp":{}}"#).is_err());
        assert!(parse(r#"{"job_events":{"enabeld":true}}"#).is_err());
    }

    #[test]
    fn events_need_their_secret_when_enabled() {
        let file = parse(r#"{"job_events":{"enabled":true}}"#).unwrap();
        assert!(build(&file, "data").is_err());
        let file =
            parse(r#"{"job_events":{"enabled":true,"secret_env":"ORCH_TEST_NO_SUCH_SECRET"}}"#)
                .unwrap();
        assert!(build(&file, "data").is_err());
        let off = parse(r#"{"job_events":{"enabled":false}}"#).unwrap();
        assert!(build(&off, "data").unwrap().1.is_none());
    }

    #[test]
    fn the_server_url_and_timeout_are_checked() {
        assert!(build(
            &parse(r#"{"estop":{"server_url":"ftp://x"}}"#).unwrap(),
            "d"
        )
        .is_err());
        assert!(build(
            &parse(r#"{"estop":{"fanout_timeout_seconds":0}}"#).unwrap(),
            "d"
        )
        .is_err());
        let ok = build(
            &parse(
                r#"{"estop":{"server_url":"http://127.0.0.1:18080","fanout_timeout_seconds":2}}"#,
            )
            .unwrap(),
            "d",
        )
        .unwrap()
        .0;
        assert_eq!(ok.server_url.as_deref(), Some("http://127.0.0.1:18080"));
        assert_eq!(ok.fanout_timeout, Duration::from_secs(2));
    }

    #[test]
    fn metrics_are_off_unless_the_section_enables_them() {
        assert!(!metrics_enabled(&parse("").unwrap()));
        assert!(!metrics_enabled(
            &parse(r#"{"metrics":{"enabled":false}}"#).unwrap()
        ));
        assert!(metrics_enabled(
            &parse(r#"{"metrics":{"enabled":true}}"#).unwrap()
        ));
        assert!(parse(r#"{"metrics":{"enabled":true,"bogus":1}}"#).is_err());
    }

    #[test]
    fn health_grpc_is_off_by_default_and_checked_when_on() {
        assert!(health_grpc(&parse("").unwrap(), None, None)
            .unwrap()
            .is_none());
        let off = parse(r#"{"health_grpc":{"enabled":false,"listen":"0.0.0.0:1"}}"#).unwrap();
        assert!(health_grpc(&off, None, None).unwrap().is_none());
        let on = parse(r#"{"health_grpc":{"enabled":true}}"#).unwrap();
        let cfg = health_grpc(&on, None, None).unwrap().unwrap();
        assert!(cfg.listen.ip().is_loopback() && cfg.token.is_none());
        // another address needs a token, from the API or from the section
        let open = parse(r#"{"health_grpc":{"enabled":true,"listen":"0.0.0.0:50114"}}"#).unwrap();
        assert!(health_grpc(&open, None, None).is_err());
        assert_eq!(
            health_grpc(&open, Some("t"), None)
                .unwrap()
                .unwrap()
                .token
                .as_deref(),
            Some("t")
        );
        // on loopback the token of the HTTP API is not borrowed: the watchdog dials without credentials
        assert!(health_grpc(&on, Some("t"), None)
            .unwrap()
            .unwrap()
            .token
            .is_none());
        let missing =
            parse(r#"{"health_grpc":{"enabled":true,"token_env":"ORCH_TEST_NO_SUCH_TOKEN"}}"#)
                .unwrap();
        assert!(health_grpc(&missing, Some("t"), None).is_err());
    }

    #[test]
    fn the_identity_key_stands_in_for_the_token_on_an_address_reachable_from_the_network() {
        let open = parse(r#"{"health_grpc":{"enabled":true,"listen":"0.0.0.0:50114"}}"#).unwrap();
        assert!(health_grpc(&open, None, None).is_err());
        let key: &[u8] = b"0123456789abcdef0123456789abcdef";
        let cfg = health_grpc(&open, None, Some(key)).unwrap().unwrap();
        assert!(cfg.token.is_none());
        assert_eq!(cfg.identity_key.as_deref(), Some(key));
        let loopback = parse(r#"{"health_grpc":{"enabled":true}}"#).unwrap();
        assert_eq!(
            health_grpc(&loopback, None, Some(key))
                .unwrap()
                .unwrap()
                .identity_key
                .as_deref(),
            Some(key)
        );
    }

    #[test]
    fn health_grpc_values_are_checked() {
        for bad in [
            r#"{"health_grpc":{"enabled":true,"listen":"not-an-address"}}"#,
            r#"{"health_grpc":{"enabled":true,"max_connections":0}}"#,
            r#"{"health_grpc":{"enabled":true,"max_watchers":5000}}"#,
            r#"{"health_grpc":{"enabled":true,"watch_interval_seconds":0}}"#,
            r#"{"health_grpc":{"enabled":true,"keepalive_seconds":0.5}}"#,
        ] {
            assert!(
                health_grpc(&parse(bad).unwrap(), None, None).is_err(),
                "{bad}"
            );
        }
        assert!(parse(r#"{"health_grpc":{"enabeld":true}}"#).is_err());
        let ok = parse(r#"{"health_grpc":{"enabled":true,"listen":"127.0.0.1:0","max_connections":3,"max_watchers":2,"watch_interval_seconds":2,"keepalive_seconds":10}}"#).unwrap();
        let cfg = health_grpc(&ok, None, None).unwrap().unwrap();
        assert_eq!((cfg.max_connections, cfg.max_watchers), (3, 2));
        assert_eq!(cfg.watch_interval, Duration::from_secs(2));
    }
}
