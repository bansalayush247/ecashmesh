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

#[test]
fn live_cashu_route_search_returns_two_distinct_source_mint_paths() {
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
    assert_eq!(response["live"]["graph"]["mechanisms"], json!(["cashu_lightning"]));
    assert_eq!(response["live"]["graph"]["search"]["candidates_generated"], 2);
    assert_eq!(response["live"]["graph"]["edge_count"], 2);

    let recommended = response["recommended_route"]["connector"]
        .as_str()
        .expect("recommended source connector");
    assert!(matches!(recommended, "cashu:source-a" | "cashu:source-b"));

    let alternatives = response["alternatives"].as_array().expect("alternatives");
    assert_eq!(alternatives.len(), 1);
    let alternative = alternatives[0]["connector"]
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
    assert!(observations.iter().any(|quote| {
        quote["kind"] == "destination_mint_quote" && quote["state"] == "known"
    }));

    assert_ne!(
        response["recommended_route"]["route_id"],
        response["alternatives"][0]["route_id"]
    );
}

#[test]
fn unavailable_source_mint_is_removed_without_fabricating_a_second_path() {
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
    assert_eq!(response["recommended_route"]["connector"], "cashu:source-a");
    assert!(response["alternatives"].as_array().unwrap().is_empty());
    assert_eq!(response["live"]["graph"]["search"]["candidates_generated"], 1);
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
