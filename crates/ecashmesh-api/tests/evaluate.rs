use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

struct ApiServer {
    child: Child,
    address: String,
}

impl ApiServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let address = listener
            .local_addr()
            .expect("read reserved local address")
            .to_string();
        drop(listener);

        let child = Command::new(env!("CARGO_BIN_EXE_ecashmesh-api"))
            .env("ECASHMESH_API_ADDRESS", &address)
            .env("ROUTING_MODE", "live")
            .env("ECASHMESH_CASHU_MINTS", "[]")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start API binary");
        let server = Self { child, address };
        for _ in 0..50 {
            if TcpStream::connect(&server.address).is_ok() {
                return server;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("API server did not start on {}", server.address);
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let encoded = body.to_string();
        let mut stream = TcpStream::connect(&self.address).expect("connect to API");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        stream
            .write_all(
                format!(
                    "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    self.address,
                    encoded.len(),
                    encoded
                )
                .as_bytes(),
            )
            .expect("send request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        let (head, body) = response
            .split_once("\r\n\r\n")
            .expect("HTTP response contains a body");
        let status = head
            .split_whitespace()
            .nth(1)
            .expect("HTTP response contains a status")
            .parse()
            .expect("HTTP status is numeric");
        (
            status,
            serde_json::from_str(body).unwrap_or_else(|_| json!({ "raw": body })),
        )
    }

    fn get_text(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(&self.address).expect("connect to API");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    self.address
                )
                .as_bytes(),
            )
            .expect("send GET request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        let (head, body) = response
            .split_once("\r\n\r\n")
            .expect("HTTP response contains a body");
        let status = head
            .split_whitespace()
            .nth(1)
            .expect("HTTP response contains a status")
            .parse()
            .expect("HTTP status is numeric");
        (status, body.to_owned())
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn api_root_points_to_the_react_native_reference_integration() {
    let server = ApiServer::start();
    let (status, page) = server.get_text("/");

    assert_eq!(status, 200);
    assert!(page.contains("React Native client"));
    assert!(page.contains("http://localhost:8081"));
    assert!(page.contains("/v1/routes/evaluate"));
    assert!(!page.contains("/v1/fixtures/confirm"));
}

#[test]
fn api_has_no_server_side_execution_or_fixture_endpoint() {
    let server = ApiServer::start();
    let body = json!({
        "payment": {
            "amount": 1000,
            "asset": "BTC",
            "destination": {"type": "lightning", "value": "lnbc1test"}
        },
        "quote_id": "quote_demo",
        "route_id": "route_demo"
    });

    let (execute_status, _) = server.post("/v1/payments/execute", &body);
    let (fixture_status, _) = server.post("/v1/fixtures/confirm", &body);

    assert_eq!(execute_status, 404);
    assert_eq!(fixture_status, 404);
}
