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
            .env("ROUTING_MODE", "simulator")
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
        self.post("/v1/routes/evaluate", body)
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
        stream
            .read_to_string(&mut response)
            .expect("read GET response");
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
fn api_root_points_to_the_react_native_reference_integration() {
    let server = ApiServer::start();
    let (status, page) = server.get_text("/");

    assert_eq!(status, 200);
    assert!(page.contains("React Native client"));
    assert!(page.contains("http://localhost:8081"));
    assert!(page.contains("/v1/routes/evaluate"));
}

#[test]
fn simulator_confirms_the_selected_recommendation_or_alternative_deterministically() {
    let server = ApiServer::start();
    let payment = valid_request(10_000, &json!([]));
    let (_, decision) = server.request(&payment);
    for route in [&decision["recommended_route"], &decision["alternatives"][0]] {
        let request = json!({
            "payment": payment,
            "quote_id": decision["quote_id"],
            "route_id": route["route_id"],
        });
        let (status, receipt) = server.post("/v1/simulator/confirm", &request);
        let (_, repeated) = server.post("/v1/simulator/confirm", &request);
        assert_eq!(status, 200);
        assert_eq!(receipt, repeated);
        assert_eq!(receipt["status"], "simulated_success");
        assert_eq!(receipt["simulated"], true);
        assert_eq!(receipt["route_id"], route["route_id"]);
        assert_eq!(receipt["fee"], route["fee"]);
        assert_eq!(receipt["path"], route["path"]);
        assert_eq!(receipt["amount"], 10_000);
    }
}

#[test]
fn simulator_rejects_changed_payment_fabricated_route_and_impossible_payment() {
    let server = ApiServer::start();
    let payment = valid_request(10_000, &json!([]));
    let (_, decision) = server.request(&payment);
    let original = json!({
        "payment": payment,
        "quote_id": decision["quote_id"],
        "route_id": decision["recommended_route"]["route_id"],
    });
    for field in ["amount", "destination", "route_id", "quote_id"] {
        let mut request = original.clone();
        match field {
            "amount" => request["payment"]["amount"] = json!(20_000),
            "destination" => request["payment"]["destination"]["value"] = json!("changed"),
            key => request[key] = json!("fabricated"),
        }
        let (status, error) = server.post("/v1/simulator/confirm", &request);
        assert_eq!(status, 400, "{field}");
        assert_eq!(error["error"]["code"], "VALIDATION_ERROR");
    }
    let mut request = original;
    request["payment"]["amount"] = json!(500_000);
    let (status, error) = server.post("/v1/simulator/confirm", &request);
    assert_eq!(status, 422);
    assert_eq!(error["error"]["code"], "NO_VIABLE_ROUTE");
}

#[test]
fn simulator_quote_binds_payment_fields_without_conflating_their_values() {
    let server = ApiServer::start();
    let mut payment = valid_request(10_000, &json!([]));
    payment["destination"]["value"] = json!("20000");
    let (_, decision) = server.request(&payment);
    payment["amount"] = json!(20_000);
    payment["destination"]["value"] = json!("10000");
    let (status, error) = server.post(
        "/v1/simulator/confirm",
        &json!({
            "payment": payment,
            "quote_id": decision["quote_id"],
            "route_id": decision["recommended_route"]["route_id"],
        }),
    );
    assert_eq!(status, 400);
    assert_eq!(error["error"]["code"], "VALIDATION_ERROR");
}

#[test]
fn expo_browser_preflight_allows_only_configured_origins() {
    let server = ApiServer::start();
    for origin in ["http://localhost:8081", "https://unrelated.example"] {
        let mut stream = TcpStream::connect(&server.address).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(stream, "OPTIONS /v1/routes/evaluate HTTP/1.1\r\nHost: {}\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type\r\nConnection: close\r\n\r\n", server.address).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let allowed = response
            .to_ascii_lowercase()
            .contains("access-control-allow-origin: http://localhost:8081");
        assert_eq!(allowed, origin == "http://localhost:8081");
    }
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
    assert_eq!(healthy_evidence["connector_type"], "cashu");
    assert_eq!(healthy_evidence["capabilities"]["supports_lightning"], true);
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
