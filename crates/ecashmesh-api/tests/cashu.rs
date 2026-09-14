// Reuse the existing binary HTTP harness and run the simulator regression suite
// in evaluate.rs separately.
#[path = "support/cashu_server.rs"]
mod support;

use serde_json::{Value, json};
use support::{ApiServer, MockMint};

fn request(amount: u64) -> Value {
    json!({"amount": amount, "asset": "BTC", "destination": {"type": "lightning", "value": "fixture-target"},
        "payment_intent": "send", "candidate_connectors": []})
}

#[test]
fn cashu_public_observations_flow_through_the_core_ranker() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    let (status, decision) = server.post("/v1/routes/evaluate", &request(100_000));
    assert_eq!(status, 200, "{decision}");
    assert_eq!(decision["simulated"], false);
    assert_eq!(decision["recommended_route"]["connector"], "cashu:fixture");
    assert!(decision["recommended_route"]["fee"]["amount"].is_null());
    assert!(decision["recommended_route"]["estimated_time_seconds"].is_null());
    assert_eq!(decision["evidence"][0]["liquidity"]["state"], "unknown");
    assert_eq!(decision["evidence"][0]["solvency"]["state"], "unknown");
    assert!(
        decision["risk_flags"]
            .as_array()
            .unwrap()
            .contains(&json!("unknown_fee"))
    );
    let observation = &decision["connector_observations"][0];
    assert_eq!(
        observation["metadata"]["value"]["name"],
        "Fixture Cashu Mint"
    );
    assert_eq!(observation["metadata"]["source"], "connector");
    assert_eq!(observation["health"]["source"], "observer");
    assert!(observation["input_fees"]["value"].is_array());
    assert!(
        decision["explanation"]["reasons"]
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty())
    );
    let (_, repeated) = server.post("/v1/routes/evaluate", &request(100_000));
    for field in ["score", "score_basis_points", "risk_flags"] {
        assert_eq!(
            decision["recommended_route"][field],
            repeated["recommended_route"][field]
        );
    }
    assert_eq!(decision["explanation"], repeated["explanation"]);
    let (status, rejected) = server.post(
        "/v1/simulator/confirm",
        &json!({
        "payment": request(100_000), "quote_id": decision["quote_id"],
        "route_id": decision["recommended_route"]["route_id"]}),
    );
    assert_eq!(status, 400);
    assert_eq!(rejected["error"]["code"], "VALIDATION_ERROR");
}

#[test]
fn cashu_limits_and_invalid_ids_are_structured_errors() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    for amount in [99, 200_001] {
        let (status, error) = server.post("/v1/routes/evaluate", &request(amount));
        assert_eq!(status, 422);
        assert_eq!(error["error"]["code"], "NO_VIABLE_ROUTE");
        assert!(
            error["error"]["details"]
                .to_string()
                .contains("payment amount")
        );
    }
    let mut body = request(100_000);
    body["candidate_connectors"] = json!(["cashu:unknown"]);
    let (status, error) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 400);
    assert_eq!(error["error"]["code"], "VALIDATION_ERROR");
    let (status, catalog) = server.get("/v1/connectors?amount=1000");
    assert_eq!(status, 200);
    assert_eq!(catalog["evaluated_amount_sats"], 1000);
    assert!(catalog["observations"][0]["send_unavailable_reason"].is_null());
    let (status, error) = server.get("/v1/connectors?amount=oops");
    assert_eq!(status, 400);
    assert_eq!(error["error"]["code"], "VALIDATION_ERROR");
}

#[test]
fn stale_malformed_and_unavailable_mints_remain_inspectable() {
    for mode in ["stale", "malformed", "unavailable", "disabled"] {
        let mint = MockMint::start(mode);
        let server = ApiServer::start(&mint.url);
        let (status, error) = server.post("/v1/routes/evaluate", &request(100_000));
        assert_eq!(status, 422, "{mode}: {error}");
        assert_eq!(error["error"]["code"], "NO_VIABLE_ROUTE");
        let (status, catalog) = server.get("/v1/connectors");
        assert_eq!(status, 200);
        let observation = &catalog["observations"][0];
        assert!(observation["send_unavailable_reason"].is_string());
        match mode {
            "stale" => assert_eq!(observation["metadata"]["state"], "stale"),
            "malformed" => assert_eq!(observation["metadata"]["state"], "unknown"),
            "unavailable" => assert_eq!(observation["availability"]["value"], false),
            "disabled" => assert_eq!(observation["melting"]["value"]["disabled"], true),
            _ => unreachable!(),
        }
    }
}
