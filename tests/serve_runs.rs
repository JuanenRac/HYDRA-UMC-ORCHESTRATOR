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
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(extra: &[&str]) -> Node {
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_hydra-umc-orchestrator"))
        .args(["serve", "--addr", "127.0.0.1", "--port", &port.to_string()])
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary must run");
    let node = Node { child, port };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return node;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the node never listened on port {port}");
}

/// One HTTP/1.1 request over a plain socket; returns the status line and the whole answer.
fn get(port: u16, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let request = format!(
        "GET {path} HTTP/1.1
Host: 127.0.0.1
Connection: close

"
    );
    stream.write_all(request.as_bytes()).expect("write");
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    let status = answer.lines().next().unwrap_or("").to_string();
    (status, answer)
}

#[test]
fn the_health_route_answers_on_a_node_that_starts() {
    let node = start(&[]);
    let (status, answer) = get(node.port, "/health");
    assert!(status.contains("200"), "{answer}");
}

#[test]
fn a_route_that_does_not_exist_is_a_404_and_the_node_keeps_answering() {
    let node = start(&[]);
    let (status, _) = get(node.port, "/no/such/route");
    assert!(status.contains("404"), "{status}");
    let (status, answer) = get(node.port, "/health");
    assert!(status.contains("200"), "{answer}");
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
