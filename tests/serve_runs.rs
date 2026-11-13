// HYDRA-UMC-ORCHESTRATOR - tests/serve_runs.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
//
// The binary when it DOES start: it listens on a free loopback port, answers over a real socket, keeps answering after
// a route that does not exist, and stops listening when it is stopped. The test waits for the listener (never a fixed
// sleep) and always stops the child.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("its address").port()
}

struct Node {
    child: Child,
    port: u16,
    log: std::path::PathBuf,
}

impl Node {
    fn said(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(extra: &[&str]) -> Node {
    let port = free_port();
    let log = std::env::temp_dir().join(format!("serve-runs-{}-{}.log", std::process::id(), port));
    let stderr_file = std::fs::File::create(&log).expect("a log file");
    let child = Command::new(env!("CARGO_BIN_EXE_hydra-umc-orchestrator"))
        .args(["serve", "--addr", "127.0.0.1", "--port", &port.to_string()])
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .expect("the binary must run");
    let node = Node { child, port, log };
    // ready = it ANSWERS a request (a node may bind first and finish its start after), not only accepts a connection
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Some(status) = try_get(port, "/") {
            if status.starts_with("HTTP/") {
                return node;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("the node never answered on port {port}: {}", node.said());
}

/// Reads one HTTP answer without waiting for the node to close the connection: the headers, then as many body bytes as
/// Content-Length says. Whatever arrived before the timeout is returned (lossy), so a missing answer shows up as empty.
fn read_answer(stream: &mut TcpStream) -> String {
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let head = text[..end].to_ascii_lowercase();
            let wanted = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if raw.len() >= end + 4 + wanted {
                break;
            }
        }
    }
    String::from_utf8_lossy(&raw).to_string()
}

/// The status line of one request, or None when the node does not answer within a few seconds.
fn try_get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let answer = read_answer(&mut stream);
    answer.lines().next().map(str::to_string)
}

/// One HTTP/1.1 request over a plain socket; returns the status line and the whole answer.
fn get(port: u16, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("timeout");
    let request = format!(
        "GET {path} HTTP/1.1
Host: 127.0.0.1
Connection: close

"
    );
    stream.write_all(request.as_bytes()).expect("write");
    let answer = read_answer(&mut stream);
    let status = answer.lines().next().unwrap_or("").to_string();
    (status, answer)
}

#[test]
fn the_health_route_answers_on_a_node_that_starts() {
    let node = start(&[]);
    let (status, answer) = get(node.port, "/health");
    assert!(
        status.contains("200"),
        "{answer} | the node said: {}",
        node.said()
    );
}

#[test]
fn a_route_that_does_not_exist_is_a_404_and_the_node_keeps_answering() {
    let node = start(&[]);
    let (status, _) = get(node.port, "/no/such/route");
    assert!(
        status.contains("404"),
        "{status} | the node said: {}",
        node.said()
    );
    let (status, answer) = get(node.port, "/health");
    assert!(
        status.contains("200"),
        "{answer} | the node said: {}",
        node.said()
    );
}

#[test]
fn a_node_that_is_stopped_stops_listening() {
    let mut node = start(&[]);
    let port = node.port;
    node.child.kill().expect("kill");
    node.child.wait().expect("wait");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut closed = false;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            closed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        closed,
        "the port {port} was still open after the node stopped"
    );
}
