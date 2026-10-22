// HYDRA-UMC-ORCHESTRATOR - src/logging.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
//
// One place for the lines this process writes to stderr. The default is the
// plain `[orchestrator] message` text it has always printed; `--log-format
// json` (or `ORCHESTRATOR_LOG_FORMAT=json`) writes one JSON object per line
// instead (`ts_ms`, `level`, `msg`), which a log collector can parse. The
// level follows the message: `fatal` -> error, `could not` / `WARNING` ->
// warn, everything else -> info.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static JSON: AtomicBool = AtomicBool::new(false);

pub fn set_json(on: bool) {
    JSON.store(on, Ordering::Relaxed);
}

pub fn is_json() -> bool {
    JSON.load(Ordering::Relaxed)
}

/// True for the values `--log-format` accepts.
pub fn parse_format(value: &str) -> Option<bool> {
    match value {
        "text" => Some(false),
        "json" => Some(true),
        _ => None,
    }
}

fn level_of(message: &str) -> &'static str {
    if message.starts_with("fatal") {
        "error"
    } else if message.contains("could not") || message.contains("WARNING") {
        "warn"
    } else {
        "info"
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The line `emit` writes, without writing it.
pub fn format_line(message: &str, json: bool, ts_ms: u64) -> String {
    if json {
        serde_json::json!({"ts_ms": ts_ms, "level": level_of(message), "msg": message}).to_string()
    } else {
        format!("[orchestrator] {message}")
    }
}

pub fn emit(message: &str) {
    eprintln!("{}", format_line(message, is_json(), now_ms()));
}

/// Same arguments as `eprintln!`, minus the `[orchestrator] ` prefix.
#[macro_export]
macro_rules! log_line {
    ($($arg:tt)*) => {
        $crate::logging::emit(&format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keeps_the_prefix_the_logs_always_had() {
        assert_eq!(format_line("hello", false, 5), "[orchestrator] hello");
    }

    #[test]
    fn json_is_one_parseable_object_per_line_with_a_level() {
        let line = format_line("could not persist \"x\"", true, 1234);
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ts_ms"], 1234);
        assert_eq!(v["level"], "warn");
        assert_eq!(v["msg"], "could not persist \"x\"");
        let fatal: serde_json::Value =
            serde_json::from_str(&format_line("fatal: no", true, 1)).unwrap();
        assert_eq!(fatal["level"], "error");
        let info: serde_json::Value =
            serde_json::from_str(&format_line("listening", true, 1)).unwrap();
        assert_eq!(info["level"], "info");
    }

    #[test]
    fn only_text_and_json_are_formats() {
        assert_eq!(parse_format("json"), Some(true));
        assert_eq!(parse_format("text"), Some(false));
        assert_eq!(parse_format("xml"), None);
    }
}
