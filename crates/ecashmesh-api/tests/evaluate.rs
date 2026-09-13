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

fn valid_request(amount: u64, candidate_connectors: &Value) -> Value {
    json!({
        "amount": amount,
        "asset": "BTC",
        "destination": {
            "type": "lightning",
            "value": "lnbc1simulateddestination"
        },
        "payment_intent": "send",
        "candidate_connectors": candidate_connectors
    })
}

#[test]
fn evaluate_demo_payment_end_to_end_with_simulated_connectors() {
    let server = ApiServer::start();
    let request = valid_request(100_000, &json!([]));
    let (status, response) = server.request(&request);

    assert_eq!(status, 200);
    assert!(
        response["quote_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("quote_"))
    );
    assert_eq!(response["recommended_route"]["connector"], "cashu:healthy");
    assert!(
        response["alternatives"]
            .as_array()
            .is_some_and(|routes| !routes.is_empty())
    );
    assert!(
        response["recommended_route"]["route_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("route_"))
    );
    assert!(response["recommended_route"]["score"].is_u64());
    assert!(response["recommended_route"]["fee"]["amount"].is_u64());
    assert!(response["score_breakdown"]["liquidity"].is_u64());
    assert!(response["evidence"].is_array());
    let healthy_evidence = response["evidence"]
        .as_array()
        .and_then(|evidence| {
            evidence
                .iter()
                .find(|item| item["connector"] == "cashu:healthy")
        })
        .expect("healthy connector evidence is present");
    assert_eq!(healthy_evidence["liquidity"]["state"], "known");
    assert!(healthy_evidence["liquidity"]["value"]["available_sats"].is_u64());
    assert!(response["explanation"]["reasons"].is_array());
    assert!(response["expires_at"].is_string());
}

#[test]
fn risk_aware_selection_prefers_fresh_evidence_over_a_cheaper_stale_route() {
    let server = ApiServer::start();
    let request = valid_request(10_000, &json!(["cashu:cheap-stale", "cashu:healthy"]));
    let (status, response) = server.request(&request);

    assert_eq!(status, 200);
    assert_eq!(response["recommended_route"]["connector"], "cashu:healthy");
    assert_eq!(
        response["alternatives"][0]["connector"],
        "cashu:cheap-stale"
    );
    assert!(
        response["alternatives"][0]["risk_flags"]
            .as_array()
            .is_some_and(|risks| risks.iter().any(|risk| risk == "stale_liquidity"))
    );
}

#[test]
fn no_viable_route_returns_a_structured_error() {
    let server = ApiServer::start();
    let request = valid_request(500_000, &json!([]));
    let (status, response) = server.request(&request);

    assert_eq!(status, 422);
    assert_eq!(response["error"]["code"], "NO_VIABLE_ROUTE");
    assert!(response["error"]["details"].is_array());
}

#[test]
fn invalid_amount_returns_a_structured_validation_error() {
    let server = ApiServer::start();
    let request = valid_request(0, &json!(["cashu:healthy"]));
    let (status, response) = server.request(&request);

    assert_eq!(status, 400);
    assert_eq!(response["error"]["code"], "VALIDATION_ERROR");
    assert!(response["error"]["message"].is_string());
    assert!(response["error"]["details"].is_array());
}

#[test]
fn stale_evidence_is_preserved_as_a_risk_and_reduces_the_route_score() {
    let server = ApiServer::start();
    let request = valid_request(10_000, &json!(["cashu:healthy", "cashu:cheap-stale"]));
    let (status, response) = server.request(&request);

    assert_eq!(status, 200);
    let recommended_score = response["recommended_route"]["score_basis_points"]
        .as_u64()
        .expect("recommended score");
    let stale_route = response["alternatives"]
        .as_array()
        .expect("alternatives")
        .iter()
        .find(|route| route["connector"] == "cashu:cheap-stale")
        .expect("stale alternative");
    assert!(
        stale_route["risk_flags"]
            .as_array()
            .is_some_and(|risks| risks.iter().any(|risk| risk == "stale_evidence"))
    );
    assert!(
        stale_route["score_basis_points"]
            .as_u64()
            .is_some_and(|score| score < recommended_score)
    );
}

#[test]
fn evaluation_is_deterministic_for_the_same_simulator_state() {
    let server = ApiServer::start();
    let request = valid_request(10_000, &json!([]));
    let (first_status, first) = server.request(&request);
    let (second_status, second) = server.request(&request);

    assert_eq!(first_status, 200);
    assert_eq!(second_status, 200);
    assert_eq!(first, second);
}
