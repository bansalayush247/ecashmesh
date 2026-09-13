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

    fn request(&self, body: &Value) -> (u16, Value) {
        let encoded = body.to_string();
        let mut stream = TcpStream::connect(&self.address).expect("connect to API");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        stream
            .write_all(
                format!(
                    "POST /v1/routes/evaluate HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    self.address,
                    encoded.len(),
                    encoded
                )
                .as_bytes(),
            )
            .expect("send evaluation request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read evaluation response");
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
            serde_json::from_str(body).expect("response body is JSON"),
        )
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn valid_request() -> Value {
    json!({
        "amount": 10000,
        "currency": "sat",
        "asset": "bitcoin",
        "payment_intent": "demo-lightning-invoice",
        "candidate_connectors": [
            "cashu:cheap-stale",
            "cashu:low-liquidity",
            "cashu:healthy"
        ]
    })
}

#[test]
fn evaluate_demo_payment_end_to_end_with_simulated_connectors() {
    let server = ApiServer::start();
    let request = valid_request();
    let (status, response) = server.request(&request);

    assert_eq!(status, 200);
    assert_eq!(response["payment"]["amount_sats"], 10000);
    assert_eq!(
        response["recommended_route"]["connectors"][0],
        "cashu:healthy"
    );
    assert_eq!(
        response["ranked_alternatives"][0]["connectors"][0],
        "cashu:cheap-stale"
    );
    assert_eq!(
        response["rejected_routes"][0]["connectors"][0],
        "cashu:low-liquidity"
    );
    assert!(response["recommended_route"]["score"].is_u64());
    assert!(response["recommended_route"]["estimated_fee_sats"].is_u64());
    assert!(response["recommended_route"]["risk_codes"].is_array());
    assert_eq!(
        response["explanation"]["recommendation"]["connectors"][0],
        "cashu:healthy"
    );
    assert!(response["explanation"]["alternatives"][0]["reasons_not_selected"].is_array());
}

#[test]
fn evaluate_returns_structured_validation_errors() {
    let server = ApiServer::start();
    let request = json!({
        "amount": 0,
        "currency": "sat",
        "asset": "bitcoin",
        "destination": "demo-destination",
        "candidate_connectors": ["cashu:healthy"]
    });
    let (status, response) = server.request(&request);

    assert_eq!(status, 400);
    assert_eq!(response["error"]["code"], "invalid_amount");
    assert!(response["error"]["message"].is_string());
}
