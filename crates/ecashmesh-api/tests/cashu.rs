// Reuse the existing binary HTTP harness and run the simulator regression suite
// in evaluate.rs separately.
#[path = "support/cashu_server.rs"]
mod support;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use support::{ApiServer, MockMint};

fn request(amount: u64) -> Value {
    json!({"amount": amount, "asset": "BTC", "destination": {"type": "lightning", "value": "lnbc1000u1qqqqqqq9kvtew"},
        "payment_intent": "send", "candidate_connectors": []})
}

#[test]
fn cashu_public_observations_flow_through_the_core_ranker() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    let (status, decision) = server.post("/v1/routes/evaluate", &request(100_000));
    assert_eq!(status, 200, "{decision}");
    assert_eq!(decision["simulated"], false);
    assert_eq!(decision["mode"], "live");
    assert_eq!(decision["recommended_route"]["connector"], "cashu:fixture");
    assert_eq!(decision["recommended_route"]["fee"]["amount"], 321);
    assert!(decision["recommended_route"]["estimated_time_seconds"].is_null());
    assert_eq!(decision["evidence"][0]["liquidity"]["state"], "unknown");
    assert_eq!(decision["evidence"][0]["solvency"]["state"], "unknown");
    assert!(
        !decision["risk_flags"]
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
    let (status, receipt) = server.post(
        "/v1/simulator/confirm",
        &json!({
        "payment": request(100_000), "quote_id": decision["quote_id"],
        "route_id": decision["recommended_route"]["route_id"]}),
    );
    assert_eq!(status, 403);
    assert_eq!(receipt["error"]["code"], "PAYMENT_SAFETY");
    assert!(
        receipt["error"]["message"]
            .to_string()
            .contains("unavailable")
    );
}

#[test]
fn cashu_destination_quote_creates_only_a_cashu_to_lightning_live_edge() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    let destination = format!(
        "cashu://request?mint={}",
        mint.url.replace(':', "%3A").replace('/', "%2F")
    );
    let body = json!({
        "amount": 100_000,
        "asset": "BTC",
        "destination": {"type": "cashu", "value": destination},
        "payment_intent": "send",
        "candidate_connectors": [],
        "source_connector": "cashu:fixture"
    });
    let (status, decision) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 200, "{decision}");
    assert_eq!(decision["mode"], "live");
    assert_eq!(
        decision["live"]["graph"]["mechanisms"],
        json!(["cashu_lightning"])
    );
    assert_eq!(
        decision["live"]["graph"]["route_shape"],
        "Cashu source → Lightning invoice → Cashu destination quote"
    );
    assert!(
        decision["live"]["quote_observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|quote| quote["kind"] == "destination_mint_quote" && quote["state"] == "known")
    );
    assert!(
        decision["live"]["quote_observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|quote| quote["kind"] == "source_melt_quote" && quote["state"] == "known")
    );
}

#[test]
fn nut18_destination_uses_its_extracted_mint_for_quote_backed_route_discovery() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    let body = json!({
        "amount": 100_000,
        "asset": "BTC",
        "destination": {"type": "cashu", "value": nut18_request(&mint.url, 100_000)},
        "payment_intent": "send",
        "candidate_connectors": [],
        "source_connector": "cashu:fixture"
    });
    let (status, decision) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 200, "{decision}");
    assert_eq!(
        decision["live"]["graph"]["destination"]["normalization"],
        "nut18_creq"
    );
    assert_eq!(
        decision["live"]["graph"]["destination"]["mint_url"],
        mint.url
    );
    assert_eq!(
        decision["live"]["graph"]["mechanisms"],
        json!(["cashu_lightning"])
    );
    assert!(
        decision["live"]["quote_observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|quote| quote["kind"] == "destination_mint_quote")
    );
    assert_eq!(
        decision["recommended_route"]["fee"]["estimate_kind"],
        "reserve_estimate"
    );
    assert_eq!(
        decision["recommended_route"]["fee"]["fee_reserve_sats"],
        321
    );
    assert_eq!(
        decision["recommended_route"]["fee"]["estimated_fee_sats"],
        321
    );
    assert_eq!(
        decision["recommended_route"]["fee_reasonableness"],
        Value::Null
    );
    assert_eq!(
        decision["recommended_route"]["fee"]["input_fee_schedule"]["included_in_estimated_fee"],
        false
    );
}

#[test]
fn malformed_nut18_request_is_a_validation_error_not_a_cashu_token() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    let body = json!({
        "amount": 100_000,
        "asset": "BTC",
        "destination": {"type": "cashu", "value": "creqA~not-base64"},
        "payment_intent": "send",
        "candidate_connectors": []
    });
    let (status, error) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 400, "{error}");
    assert_eq!(error["error"]["code"], "VALIDATION_ERROR");
    assert!(
        error["error"]["details"]
            .to_string()
            .contains("NUT-18 Cashu payment request")
    );
    assert!(
        !error["error"]["details"]
            .to_string()
            .contains("bearer tokens")
    );
}

#[test]
fn missing_live_quote_returns_no_route_without_simulator_fallback() {
    let mint = MockMint::start("quote_unavailable");
    let server = ApiServer::start(&mint.url);
    let (status, error) = server.post("/v1/routes/evaluate", &request(100_000));
    assert_eq!(status, 422, "{error}");
    assert_eq!(error["error"]["code"], "NO_VIABLE_ROUTE");
    assert!(
        error["error"]["details"]
            .to_string()
            .contains("missing quote")
    );
}

#[test]
fn cashu_limits_and_invalid_ids_are_structured_errors() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    for amount in [99, 200_001] {
        let (status, error) = server.post("/v1/routes/evaluate", &request(amount));
        assert_eq!(status, 422);
        assert_eq!(error["error"]["code"], "NO_VIABLE_ROUTE");
        assert!(!error["error"]["details"].as_array().unwrap().is_empty());
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

#[test]
fn all_discovery_sources_merge_and_expose_complete_public_metadata() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(
        &json!([{"id":"cashu:fixture","url":mint.url}]),
        &json!([format!("{}directory", mint.url)]),
        &json!([]),
    );
    let mut payment = request(100_000);
    payment["wallet_mint_urls"] = json!([mint.url.trim_end_matches('/')]);
    payment["mint_urls"] = json!([mint.url]);
    payment["destination"]["mint_url"] = json!(mint.url);
    let (status, decision) = server.post("/v1/routes/evaluate", &payment);
    assert_eq!(status, 200, "{decision}");
    assert_eq!(decision["discovery"]["mints"].as_array().unwrap().len(), 1);
    let identity = &decision["discovery"]["mints"][0];
    assert_eq!(identity["canonical_url"], mint.url);
    assert_eq!(identity["provenance"].as_array().unwrap().len(), 5);
    let observation = &decision["connector_observations"][0];
    assert_eq!(observation["public_key"]["state"], "known");
    assert_eq!(
        observation["denominations"]["value"][0]["amounts"],
        json!([1, 2])
    );
    assert_eq!(
        observation["supported_units"]["value"],
        json!(["sat", "usd"])
    );
    assert_eq!(
        observation["supported_nuts"]["value"]["999"]["supported"],
        true
    );
    payment["candidate_connectors"] = json!([identity["canonical_id"]]);
    let (status, alias_decision) = server.post("/v1/routes/evaluate", &payment);
    assert_eq!(status, 200);
    assert_eq!(
        alias_decision["recommended_route"]["connector"],
        "cashu:fixture"
    );
}

#[test]
fn wallet_and_directory_can_discover_mints_without_seeds() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(&json!([]), &json!([]), &json!([mint.url]));
    let mut payment = request(100_000);
    payment["wallet_mint_urls"] = json!([mint.url]);
    let (status, decision) = server.post("/v1/routes/evaluate", &payment);
    assert_eq!(status, 200, "{decision}");
    assert!(
        decision["discovery"]["mints"][0]["aliases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, empty) = server.get("/v1/connectors");
    assert_eq!(status, 200);
    assert!(empty["observations"].as_array().unwrap().is_empty());
    let directory_server = ApiServer::start_with_sources(
        &json!([]),
        &json!([format!("{}directory", mint.url)]),
        &json!([mint.url]),
    );
    let (status, catalog) = directory_server.get("/v1/connectors");
    assert_eq!(status, 200);
    assert_eq!(catalog["observations"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["discovery"]["mints"][0]["provenance"][0]["source"]["type"],
        "directory"
    );
}

#[test]
fn discovery_errors_are_inspectable_and_untrusted_private_targets_are_blocked() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(&json!([]), &json!([]), &json!([]));
    let mut payment = request(100_000);
    payment["wallet_mint_urls"] = json!(["file:///etc/passwd", mint.url]);
    let (status, catalog) = server.post("/v1/connectors/discover", &payment);
    assert_eq!(status, 200);
    assert!(
        catalog["discovery"]["issues"]
            .to_string()
            .contains("INVALID_MINT_URL")
    );
    assert!(
        catalog["observations"][0]["issues"]
            .to_string()
            .contains("non-public")
    );
    let (status, error) = server.post("/v1/routes/evaluate", &payment);
    assert_eq!(status, 422);
    assert_eq!(error["error"]["code"], "NO_VIABLE_ROUTE");
    payment["wallet_mint_urls"] = json!(vec![mint.url.clone(); 65]);
    assert_eq!(server.post("/v1/connectors/discover", &payment).0, 400);
}

fn nut18_request(mint_url: &str, amount: u64) -> String {
    let payload = cbor_map(vec![
        ("a", cbor_uint(amount)),
        ("u", cbor_text("sat")),
        ("m", cbor_array(vec![cbor_text(mint_url)])),
        (
            "t",
            cbor_array(vec![cbor_map(vec![
                ("t", cbor_text("post")),
                ("a", cbor_text("https://receiver.example/cashu")),
            ])]),
        ),
    ]);
    format!("creqA{}", URL_SAFE_NO_PAD.encode(payload))
}

fn cbor_map(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut value = cbor_head(5, entries.len() as u64);
    for (key, entry) in entries {
        value.extend(cbor_text(key));
        value.extend(entry);
    }
    value
}

fn cbor_array(entries: Vec<Vec<u8>>) -> Vec<u8> {
    let mut value = cbor_head(4, entries.len() as u64);
    for entry in entries {
        value.extend(entry);
    }
    value
}

fn cbor_text(value: &str) -> Vec<u8> {
    let mut encoded = cbor_head(3, value.len() as u64);
    encoded.extend(value.as_bytes());
    encoded
}

fn cbor_uint(value: u64) -> Vec<u8> {
    cbor_head(0, value)
}

fn cbor_head(major: u8, value: u64) -> Vec<u8> {
    let tag = major << 5;
    match value {
        0..=23 => vec![tag | u8::try_from(value).expect("range is bounded")],
        24..=255 => vec![tag | 0x18, u8::try_from(value).expect("range is bounded")],
        256..=65_535 => {
            let mut encoded = vec![tag | 0x19];
            encoded.extend(
                u16::try_from(value)
                    .expect("range is bounded")
                    .to_be_bytes(),
            );
            encoded
        }
        65_536..=4_294_967_295 => {
            let mut encoded = vec![tag | 0x1a];
            encoded.extend(
                u32::try_from(value)
                    .expect("range is bounded")
                    .to_be_bytes(),
            );
            encoded
        }
        _ => {
            let mut encoded = vec![tag | 0x1b];
            encoded.extend(value.to_be_bytes());
            encoded
        }
    }
}
