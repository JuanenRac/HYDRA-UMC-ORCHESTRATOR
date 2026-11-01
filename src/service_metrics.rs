// =============================================================================
// HYDRA-UMC-SDK - tools/canonical/service_metrics.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The `GET /metrics` of a service that answers with `tiny_http`, written once.
//!
//! This file has a single canonical copy (`tools/canonical/service_metrics.rs` of HYDRA-UMC-SDK) that is vendored, byte for byte, into the Rust services;
//! `tools/sync_repo_tools.py` keeps the copies identical. It is the Rust twin of `service_metrics.py`: the same names, the same series and the same rules.
//! A service adopts it in four steps:
//!
//! 1. `mod service_metrics;` in `main.rs`, and `static METRICS: ServiceMetrics = ServiceMetrics::new("hydra-umc-example");` next to its server;
//! 2. `METRICS.start()` when the server starts;
//! 3. `METRICS.count(status)` wherever an answer is written;
//! 4. `if service_metrics::is_metrics_request(&request) { METRICS.respond(request, version, authorized, &checks); continue; }` before the routes.
//!
//! It publishes the text format of Prometheus (version 0.0.4 of the exposition format) and only numbers about the service: its version, how long it has been
//! up, how many responses it gave in each class of status (2xx, 3xx, 4xx, 5xx) and its health, the aggregate of the checks the service declares (a list of check
//! name and whether it passes; no checks means the process answers, so healthy). Nothing a request carried is ever a label or a value: no path, no address, no
//! token, no body. `authorized` says whether the caller may read the numbers; a service with a token gives the same answer it gives to its most sensitive read
//! (401 without it), and a service with none publishes them like its `/stats`.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// The classes of status the responses are counted in, in the order of the counters.
const STATUS_CLASSES: [&str; 4] = ["2xx", "3xx", "4xx", "5xx"];

/// A check name as a label value: lower case letters, digits and underscores, so a check can never carry free text.
pub fn check_name(name: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "check".to_string()
    } else {
        trimmed.to_string()
    }
}

fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

/// The counters of one service; safe to use from the threads of the HTTP server.
pub struct ServiceMetrics {
    service: &'static str,
    started: OnceLock<Instant>,
    responses: [AtomicU64; 4],
}

impl ServiceMetrics {
    pub const fn new(service: &'static str) -> Self {
        ServiceMetrics {
            service,
            started: OnceLock::new(),
            responses: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }

    /// Mark the start of the process (the first call wins).
    pub fn start(&self) {
        let _ = self.started.get_or_init(Instant::now);
    }

    /// Count the answer of a request by the class of its status; a status outside 2xx-4xx counts as 5xx.
    pub fn count(&self, status: u16) {
        self.start();
        let index = match status / 100 {
            2 => 0,
            3 => 1,
            4 => 2,
            _ => 3,
        };
        self.responses[index].fetch_add(1, Ordering::Relaxed);
    }

    /// The text of the metrics; `checks` are the parts of the health of the service and whether each passes.
    pub fn render(&self, version: &str, checks: &[(&str, bool)]) -> String {
        let service = label(self.service);
        let uptime = self.started.get_or_init(Instant::now).elapsed().as_secs();
        let mut results: Vec<(String, bool)> = checks
            .iter()
            .map(|(name, ok)| (check_name(name), *ok))
            .collect();
        results.sort();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# HELP hydra_umc_service_info The version of the running service."
        );
        let _ = writeln!(out, "# TYPE hydra_umc_service_info gauge");
        let _ = writeln!(
            out,
            "hydra_umc_service_info{{service=\"{service}\",version=\"{}\"}} 1",
            label(version)
        );
        let _ = writeln!(
            out,
            "# HELP hydra_umc_service_uptime_seconds Seconds since the process started."
        );
        let _ = writeln!(out, "# TYPE hydra_umc_service_uptime_seconds gauge");
        let _ = writeln!(
            out,
            "hydra_umc_service_uptime_seconds{{service=\"{service}\"}} {uptime}"
        );
        let _ = writeln!(
            out,
            "# HELP hydra_umc_service_http_responses_total Responses given, by the class of the status."
        );
        let _ = writeln!(out, "# TYPE hydra_umc_service_http_responses_total counter");
        for (class, counter) in STATUS_CLASSES.iter().zip(&self.responses) {
            let _ = writeln!(
                out,
                "hydra_umc_service_http_responses_total{{service=\"{service}\",class=\"{class}\"}} {}",
                counter.load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "# HELP hydra_umc_service_health 1 when every check of the service passes (or it declares none), 0 when one fails."
        );
        let _ = writeln!(out, "# TYPE hydra_umc_service_health gauge");
        let _ = writeln!(
            out,
            "hydra_umc_service_health{{service=\"{service}\"}} {}",
            u8::from(results.iter().all(|(_, ok)| *ok))
        );
        if !results.is_empty() {
            let _ = writeln!(
                out,
                "# HELP hydra_umc_service_health_check 1 when one check of the service passes."
            );
            let _ = writeln!(out, "# TYPE hydra_umc_service_health_check gauge");
            for (name, ok) in &results {
                let _ = writeln!(
                    out,
                    "hydra_umc_service_health_check{{service=\"{service}\",check=\"{name}\"}} {}",
                    u8::from(*ok)
                );
            }
        }
        out
    }

    /// Answer `GET /metrics` (the request is used up): the numbers when `authorized`, `401` otherwise. The answer is counted like any other.
    pub fn respond(
        &self,
        request: tiny_http::Request,
        version: &str,
        authorized: bool,
        checks: &[(&str, bool)],
    ) {
        let header = |name: &str, value: &str| {
            tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
        };
        let response = if authorized {
            self.count(200);
            tiny_http::Response::from_string(self.render(version, checks))
                .with_status_code(200)
                .with_header(header(
                    "Content-Type",
                    "text/plain; version=0.0.4; charset=utf-8",
                ))
        } else {
            self.count(401);
            tiny_http::Response::from_string(r#"{"error": "unauthorized"}"#)
                .with_status_code(401)
                .with_header(header("Content-Type", "application/json"))
                .with_header(header("WWW-Authenticate", "Bearer"))
        };
        let _ = request.respond(response);
    }
}

/// True for `GET /metrics` (a query string does not matter).
pub fn is_metrics_request(request: &tiny_http::Request) -> bool {
    request.method() == &tiny_http::Method::Get
        && request.url().split('?').next() == Some("/metrics")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape the exposition format asks for: every line is a comment (`# HELP`/`# TYPE`) or `name{labels} value`, a name is declared before it is
    /// used and every value is a number.
    fn assert_valid_exposition(text: &str) {
        let mut declared = std::collections::BTreeSet::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let mut parts = rest.split_whitespace();
                let name = parts.next().expect("a name");
                let kind = parts.next().expect("a type");
                assert!(["counter", "gauge", "histogram", "summary", "untyped"].contains(&kind));
                declared.insert(name.to_string());
            } else if line.starts_with("# HELP ") {
                assert!(line.split_whitespace().count() >= 4, "{line}");
            } else if !line.is_empty() {
                let (series, value) = line.rsplit_once(' ').expect("a value");
                let name = series.split('{').next().unwrap();
                assert!(declared.contains(name), "{name} used before its TYPE line");
                assert!(value.parse::<f64>().is_ok(), "{line}");
            }
        }
    }

    #[test]
    fn the_text_is_valid_prometheus_and_counts_by_class() {
        let metrics = ServiceMetrics::new("hydra-umc-example");
        metrics.start();
        for status in [200, 204, 301, 404, 401, 503, 101] {
            metrics.count(status);
        }
        let text = metrics.render("1.2.3", &[]);
        assert_valid_exposition(&text);
        assert!(text
            .contains("hydra_umc_service_info{service=\"hydra-umc-example\",version=\"1.2.3\"} 1"));
        for (class, expected) in [("2xx", 2), ("3xx", 1), ("4xx", 2), ("5xx", 2)] {
            let series = format!(
                "hydra_umc_service_http_responses_total{{service=\"hydra-umc-example\",class=\"{class}\"}} {expected}"
            );
            assert!(text.contains(&series), "{series}");
        }
        assert!(text.contains("hydra_umc_service_health{service=\"hydra-umc-example\"} 1"));
        assert!(!text.contains("hydra_umc_service_health_check"));
    }

    #[test]
    fn health_is_the_aggregate_of_the_checks_and_a_name_cannot_carry_free_text() {
        let metrics = ServiceMetrics::new("hydra-umc-example");
        let text = metrics.render(
            "1.0.0",
            &[("Registry ok", true), ("store/disk \"x\"", false)],
        );
        assert_valid_exposition(&text);
        assert!(text.contains("hydra_umc_service_health{service=\"hydra-umc-example\"} 0"));
        assert!(text.contains("check=\"registry_ok\"} 1"));
        assert!(text.contains("check=\"store_disk_x\"} 0"));
        assert_eq!(check_name("***"), "check");
        assert_eq!(check_name("A b__c"), "a_b__c");
    }

    #[test]
    fn a_label_is_escaped() {
        assert_eq!(label("a\"b\\c\nd"), "a\\\"b\\\\c d");
    }
}
