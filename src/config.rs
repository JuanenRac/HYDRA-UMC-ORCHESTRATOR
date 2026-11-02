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
use crate::job_events::JobEventsConfig;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub estop: Option<EstopSection>,
    pub job_events: Option<JobEventsSection>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
