#[path = "support/cashu_server.rs"]
mod support;

use serde_json::{Value, json};
use support::{ApiServer, MockMint};

fn cashu_destination(mint_url: &str) -> String {
    format!(
        "cashu://request?mint={}",
        mint_url.replace(':', "%3A").replace('/', "%2F")
    )
}

fn payment(destination: String, amount: u64) -> Value {
    json!({
        "amount": amount,
        "asset": "BTC",
        "destination": {"type": "cashu", "value": destination},
        "payment_intent": "send",
        "candidate_connectors": ["cashu:source-a", "cashu:source-b"]
    })
}

fn lightning_payment(amount: u64) -> Value {
    json!({
        "amount": amount,
        "asset": "BTC",
        "destination": {"type": "lightning", "value": "lnbc1000u1qqqqqqq9kvtew"},
        "payment_intent": "send",
        "candidate_connectors": ["cashu:source-a", "cashu:source-b"]
    })
}

#[test]
fn lightning_invoice_returns_two_independent_cashu_sources() {
    let source_a = MockMint::start("healthy");
    let source_b = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(
        &json!([
            {"id":"cashu:source-a","url":source_a.url},
            {"id":"cashu:source-b","url":source_b.url}
        ]),
        &json!([]),
        &json!([]),
    );

    let (status, response) = server.post("/v1/routes/evaluate", &lightning_payment(100_000));

    assert_eq!(status, 200, "{response}");
    assert_eq!(response["recommended_source"]["protocol"], "cashu");
    assert_eq!(
        response["recommended_source"]["settlement_mechanism"],
        "cashu_lightning"
    );
    assert_eq!(response["alternative_sources"].as_array().unwrap().len(), 1);
    assert_ne!(
        response["recommended_source"]["source_id"],
        response["alternative_sources"][0]["source_id"]
    );
}

#[test]
fn live_cashu_source_selection_returns_two_independent_sources() {
    let source_a = MockMint::start("healthy");
    let source_b = MockMint::start("healthy");
    let destination = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(
        &json!([
            {"id":"cashu:source-a","url":source_a.url},
            {"id":"cashu:source-b","url":source_b.url},
            {"id":"cashu:destination","url":destination.url}
        ]),
        &json!([]),
        &json!([]),
    );

    let (status, response) = server.post(
        "/v1/routes/evaluate",
        &payment(cashu_destination(&destination.url), 100_000),
    );

    assert_eq!(status, 200, "{response}");
    assert_eq!(
        response["live"]["graph"]["mechanisms"],
        json!(["cashu_lightning"])
    );
    assert_eq!(
        response["live"]["graph"]["search"]["candidates_generated"],
        2
    );
    assert_eq!(response["live"]["graph"]["edge_count"], 2);

    let recommended = response["recommended_source"]["source_id"]
        .as_str()
        .expect("recommended source connector");
    assert!(matches!(recommended, "cashu:source-a" | "cashu:source-b"));

    let alternatives = response["alternative_sources"]
        .as_array()
        .expect("alternative sources");
    assert_eq!(alternatives.len(), 1);
    let alternative = alternatives[0]["source_id"]
        .as_str()
        .expect("alternative source connector");
    assert!(matches!(alternative, "cashu:source-a" | "cashu:source-b"));
    assert_ne!(recommended, alternative);

    let observations = response["live"]["quote_observations"]
        .as_array()
        .expect("quote observations");
    let source_melt_quotes = observations
        .iter()
        .filter(|quote| quote["kind"] == "source_melt_quote" && quote["state"] == "known")
        .count();
    assert_eq!(source_melt_quotes, 2);
    assert!(
        observations.iter().any(|quote| {
            quote["kind"] == "destination_mint_quote" && quote["state"] == "known"
        })
    );

    assert_ne!(
        response["recommended_source"]["route_id"],
        response["alternative_sources"][0]["route_id"]
    );
    for source in std::iter::once(&response["recommended_source"])
        .chain(response["alternative_sources"].as_array().unwrap())
    {
        assert_eq!(source["protocol"], "cashu");
        assert_eq!(source["settlement_mechanism"], "cashu_lightning");
        assert_eq!(source["executable"], false);
        assert_eq!(source["route_classification"], "quote_backed");
        assert_eq!(source["path"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn unavailable_source_is_excluded_without_fabricating_a_mint_bridge() {
    let source_a = MockMint::start("healthy");
    let source_b = MockMint::start("unavailable");
    let destination = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(
        &json!([
            {"id":"cashu:source-a","url":source_a.url},
            {"id":"cashu:source-b","url":source_b.url},
            {"id":"cashu:destination","url":destination.url}
        ]),
        &json!([]),
        &json!([]),
    );

    let (status, response) = server.post(
        "/v1/routes/evaluate",
        &payment(cashu_destination(&destination.url), 100_000),
    );

    assert_eq!(status, 200, "{response}");
    assert_eq!(
        response["recommended_source"]["source_id"],
        "cashu:source-a"
    );
    assert!(
        response["alternative_sources"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        response["live"]["graph"]["search"]["candidates_generated"],
        1
    );
    assert_eq!(response["live"]["graph"]["edge_count"], 1);
    assert!(
        response["live"]["quote_observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|quote| quote["connector"] == "cashu:source-a"
                && quote["kind"] == "source_melt_quote"
                && quote["state"] == "known")
    );
}

#[test]
fn explicitly_selected_wallet_sources_exclude_other_configured_mints() {
    let source_a = MockMint::start("healthy");
    let source_b = MockMint::start("healthy");
    let source_c = MockMint::start("healthy");
    let destination = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(
        &json!([
            {"id":"cashu:source-a","url":source_a.url},
            {"id":"cashu:source-b","url":source_b.url},
            {"id":"cashu:source-c","url":source_c.url}
        ]),
        &json!([]),
        &json!([destination.url]),
    );
    let body = json!({
        "amount": 100_000,
        "asset": "BTC",
        "destination": {"type":"cashu", "value":cashu_destination(&destination.url)},
        "payment_intent":"send",
        "wallet_mint_urls":[source_a.url, source_b.url]
    });
    let (status, response) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 200, "{response}");
    let sources = std::iter::once(&response["recommended_source"])
        .chain(response["alternative_sources"].as_array().unwrap())
        .map(|route| route["source_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().all(|source| *source != "cashu:source-c"));
}
