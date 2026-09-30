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

fn payment(destination: &str, amount: u64) -> Value {
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
fn strict_empty_registry_never_evaluates_seed_sources() {
    let mint = MockMint::start("healthy");
    let server = ApiServer::start(&mint.url);
    let mut body = lightning_payment(100_000);
    body.as_object_mut().unwrap().remove("candidate_connectors");
    body["strict_source_registry"] = json!(true);
    body["wallet_mint_urls"] = json!([]);
    body["federation_connector_ids"] = json!([]);
    let (status, result) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 422, "{result}");
    assert_eq!(result["error"]["code"], "NO_VIABLE_ROUTE");
    assert_eq!(
        result["error"]["diagnostics"]["quote_observations"],
        json!([])
    );
}

#[test]
fn native_bridge_competes_with_cashu_and_excludes_invalid_evidence() {
    let cashu = MockMint::start("healthy");
    for mode in [
        "healthy",
        "fed_insufficient",
        "fed_stale",
        "fed_unpatched",
        "fed_unauthorized",
        "fed_empty",
    ] {
        let fed = MockMint::start(mode);
        let server = ApiServer::start_with_federations(
            &json!([{"id":"cashu:source-a", "url":cashu.url}]),
            &json!([]),
            &json!([]),
            &json!([{"id":"fedimint:native", "label":"Native", "federation_id":"11".repeat(32),
                "bridge_url":fed.url, "quote_backend":"local_v0121_bridge"}]),
        );
        let mut body = lightning_payment(100_000);
        body.as_object_mut().unwrap().remove("candidate_connectors");
        body["strict_source_registry"] = json!(true);
        body["wallet_mint_urls"] = json!([cashu.url]);
        body["federation_connector_ids"] = json!(["fedimint:native"]);
        let (status, result) = server.post("/v1/routes/evaluate", &body);
        assert_eq!(status, 200, "{result}");
        let routes: Vec<_> = std::iter::once(&result["recommended_source"])
            .chain(result["alternative_sources"].as_array().unwrap())
            .collect();
        let native = routes.iter().find(|route| route["protocol"] == "fedimint");
        if mode == "healthy" {
            let native = native.expect("native bridge participates in normal comparison");
            assert_eq!(native["executable"], false);
            assert_eq!(native["route_classification"], "quote_backed");
            assert_eq!(native["gateway_status"], "online");
            assert!(native["available_gateway_count"].is_null());
            let evidence = result["live"]["quote_observations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|q| q["kind"] == "fedimint_lightning_fee_quote")
                .unwrap();
            assert_eq!(evidence["value"]["total_fee_sats"], 3);
            assert_eq!(evidence["value"]["native_evidence"]["total_fee_msat"], 2002);
        } else {
            assert!(native.is_none(), "{result}");
            assert!(
                result["excluded_sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["source_id"] == "fedimint:native")
            );
        }
    }
}

#[test]
fn missing_quote_bridge_survives_no_route_as_structured_diagnostics() {
    let fed = MockMint::start("healthy");
    let server = ApiServer::start_with_federations(
        &json!([]),
        &json!([]),
        &json!([]),
        &json!([
            {"id":"fedimint:no-bridge", "label":"No bridge", "federation_id":"11".repeat(32), "bridge_url":fed.url}
        ]),
    );
    let mut body = lightning_payment(100_000);
    body.as_object_mut().unwrap().remove("candidate_connectors");
    body["strict_source_registry"] = json!(true);
    body["federation_connector_ids"] = json!(["fedimint:no-bridge", "fedimint:not-configured"]);
    let (status, result) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 422, "{result}");
    let excluded = result["error"]["diagnostics"]["excluded_sources"]
        .as_array()
        .unwrap();
    assert_eq!(excluded.len(), 2);
    assert!(excluded.iter().any(|row| {
        row["reason"]
            .as_str()
            .unwrap()
            .contains("No read-only Fedimint quote bridge")
    }));
    assert!(
        excluded
            .iter()
            .any(|row| row["reason"].as_str().unwrap().contains("not configured"))
    );
}

#[test]
fn restored_alias_cannot_select_a_different_federation() {
    let fed = MockMint::start("healthy");
    let server = ApiServer::start_with_federations(
        &json!([]),
        &json!([]),
        &json!([]),
        &json!([
            {"id":"fedimint:alias", "label":"Local", "federation_id":"11".repeat(32), "bridge_url":fed.url, "quote_backend":"local_v0121_bridge"}
        ]),
    );
    let mut body = lightning_payment(100_000);
    body.as_object_mut().unwrap().remove("candidate_connectors");
    body["strict_source_registry"] = json!(true);
    body["federation_connector_ids"] = json!(["fedimint:alias"]);
    body["federation_identities"] = json!({"fedimint:alias":"22".repeat(32)});
    let (status, result) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 422, "{result}");
    assert_eq!(
        result["error"]["diagnostics"]["quote_observations"],
        json!([])
    );
    assert!(
        result["error"]["diagnostics"]["excluded_sources"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("different federation")
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One mixed-source scenario checks both native destination mechanisms and exclusion.
fn automatic_five_cashu_three_fedimint_for_lightning_and_cashu_destination() {
    // Four viable Cashu sources, one unsupported; two quote-backed federations,
    // one without a bridge. All fixtures bind loopback; no public funds/services.
    let cashu: Vec<_> = ["healthy", "healthy", "healthy", "healthy", "disabled"]
        .into_iter()
        .map(MockMint::start)
        .collect();
    let feds: Vec<_> = (0..3).map(|_| MockMint::start("healthy")).collect();
    let destination = MockMint::start("healthy");
    let seeds: Vec<_> = cashu
        .iter()
        .enumerate()
        .map(|(i, mint)| json!({"id":format!("cashu:source-{i}"),"url":mint.url}))
        .collect();
    let federations: Vec<_> = feds.iter().enumerate().map(|(i, fed)| {
        let mut entry = json!({"id":format!("fedimint:source-{i}"),"label":format!("Fed {i}"),"federation_id":format!("{i:064x}"),"bridge_url":fed.url});
        if i < 2 { entry["quote_backend"] = json!("local_v0121_bridge"); }
        entry
    }).collect();
    let server = ApiServer::start_with_federations(
        &json!(seeds),
        &json!([]),
        &json!([destination.url]),
        &json!(federations),
    );
    for cashu_target in [false, true] {
        let mut body = lightning_payment(100_000);
        body.as_object_mut().unwrap().remove("candidate_connectors");
        body["strict_source_registry"] = json!(true);
        body["wallet_mint_urls"] = json!(
            cashu
                .iter()
                .map(|mint| mint.url.clone())
                .collect::<Vec<_>>()
        );
        body["federation_connector_ids"] = json!([
            "fedimint:source-0",
            "fedimint:source-1",
            "fedimint:source-2"
        ]);
        if cashu_target {
            body["destination"] =
                json!({"type":"cashu", "value":cashu_destination(&destination.url)});
        }
        let (status, result) = server.post("/v1/routes/evaluate", &body);
        assert_eq!(status, 200, "{result}");
        let routes: Vec<_> = std::iter::once(&result["recommended_source"])
            .chain(result["alternative_sources"].as_array().unwrap())
            .collect();
        assert_eq!(routes.len(), 6, "{result}");
        assert_eq!(
            routes
                .iter()
                .filter(|r| r["protocol"] == "fedimint")
                .count(),
            2
        );
        assert_eq!(
            routes.iter().filter(|r| r["protocol"] == "cashu").count(),
            4
        );
        for route in routes {
            assert_eq!(route["executable"], false);
            assert!(matches!(
                route["settlement_mechanism"].as_str().unwrap(),
                "cashu_lightning"
                    | "fedimint_lightning"
                    | "fedimint_lightning_destination_settlement"
            ));
            assert_eq!(route["path"].as_array().unwrap().len(), 1);
        }
        let excluded = result["excluded_sources"].as_array().unwrap();
        assert_eq!(excluded.len(), 2, "{excluded:?}");
        assert!(
            excluded
                .iter()
                .any(|row| row["source_id"] == "fedimint:source-2")
        );
        assert!(
            excluded
                .iter()
                .any(|row| row["source_id"] == "cashu:source-4")
        );
        // Deliberately list the destination as a source too: it must stay excluded.
        if cashu_target {
            body["wallet_mint_urls"]
                .as_array_mut()
                .unwrap()
                .push(json!(destination.url));
            let (status, result) = server.post("/v1/routes/evaluate", &body);
            assert_eq!(status, 200, "{result}");
            assert!(
                result["excluded_sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["reason"] == "Destination is excluded from payment sources")
            );
        }
        // Restrict to one source: no disabled/unselected federation quotes.
        body["wallet_mint_urls"] = json!([cashu[0].url]);
        body["federation_connector_ids"] = json!([]);
        let (status, result) = server.post("/v1/routes/evaluate", &body);
        assert_eq!(status, 200, "{result}");
        assert_eq!(result["alternative_sources"], json!([]));
        assert!(
            result["live"]["quote_observations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["kind"] == "destination_mint_quote"
                    || row["connector"] == "cashu:source-0")
        );
    }
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
        &payment(&cashu_destination(&destination.url), 100_000),
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
        &payment(&cashu_destination(&destination.url), 100_000),
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

#[test]
fn unconfigured_user_authorized_federation_is_rejected_without_creating_a_route() {
    let source = MockMint::start("healthy");
    let server = ApiServer::start_with_sources(
        &json!([{"id":"cashu:source","url":source.url}]),
        &json!([]),
        &json!([]),
    );
    let body = json!({
        "amount": 100_000,
        "asset": "BTC",
        "destination": {"type":"lightning", "value":"lnbc1000u1qqqqqqq9kvtew"},
        "payment_intent":"send",
        "federation_connector_ids":["fedimint:not-configured"]
    });
    let (status, response) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 400, "{response}");
    assert_eq!(response["error"]["code"], "VALIDATION_ERROR");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unavailable federation"))
    );
}

#[test]
fn rejection_reason_survives_bridge_and_api_adapters() {
    let cashu = MockMint::start("healthy");
    for (mode, message) in [
        ("fed_invoice_expired", "Lightning invoice expired."),
        (
            "fed_gateway_unreachable",
            "Fedimint gateway did not respond to verification",
        ),
        (
            "fed_gateway_verification_failed",
            "Fedimint gateway identity verification failed",
        ),
        (
            "fed_invoice_amount_mismatch",
            "Lightning invoice amount is missing or does not match",
        ),
        (
            "fed_invoice_network_mismatch",
            "Lightning invoice network does not match",
        ),
    ] {
        let fed = MockMint::start(mode);
        let server = ApiServer::start_with_federations(
            &json!([{"id":"cashu:source-a", "url":cashu.url}]),
            &json!([]),
            &json!([]),
            &json!([{"id":"fedimint:native", "label":"Native", "federation_id":"11".repeat(32),
                "bridge_url":fed.url, "quote_backend":"local_v0121_bridge"}]),
        );
        let mut body = lightning_payment(100_000);
        body.as_object_mut().unwrap().remove("candidate_connectors");
        body["strict_source_registry"] = json!(true);
        body["wallet_mint_urls"] = json!([cashu.url]);
        body["federation_connector_ids"] = json!(["fedimint:native"]);
        let (status, result) = server.post("/v1/routes/evaluate", &body);
        assert_eq!(status, 200, "{result}");
        assert_eq!(result["recommended_source"]["protocol"], "cashu");
        let excluded = result["excluded_sources"].as_array().unwrap();
        let federation = excluded
            .iter()
            .find(|source| source["source_id"] == "fedimint:native")
            .unwrap();
        assert!(
            federation["reason"].as_str().unwrap().starts_with(message),
            "{result}"
        );
        assert!(
            federation["reason"]
                .as_str()
                .unwrap()
                .contains("gateway comparison unavailable"),
            "{result}"
        );
        assert!(
            !federation["reason"]
                .as_str()
                .unwrap()
                .contains("Verified gateway-fee estimate is available"),
            "{result}"
        );
    }
}

#[test]
fn comparison_ranks_unfunded_federation_even_without_a_cashu_route() {
    let fed = MockMint::start("fed_estimate");
    let cashu = MockMint::start("healthy");
    let server = ApiServer::start_with_federations(
        &json!([{"id":"cashu:source-a", "url":cashu.url}]),
        &json!([]),
        &json!([]),
        &json!([{"id":"fedimint:native", "label":"Native", "federation_id":"11".repeat(32),
            "bridge_url":fed.url, "quote_backend":"local_v0121_bridge"}]),
    );
    let mut body = lightning_payment(100_000);
    body.as_object_mut().unwrap().remove("candidate_connectors");
    body["strict_source_registry"] = json!(true);
    body["wallet_mint_urls"] = json!([]);
    body["federation_connector_ids"] = json!(["fedimint:native"]);
    let (status, _) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(
        status, 422,
        "normal evaluation still excludes the unfunded federation"
    );
    let (status, result) = server.post("/v1/routes/compare", &body);
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["mode"], "comparison_only");
    assert_eq!(result["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(result["candidates"][0]["source_id"], "fedimint:native");
    assert_eq!(result["candidates"][0]["federation_id"], "11".repeat(32));
    assert_eq!(result["candidates"][0]["fee_sats"], 7);
    assert_eq!(result["candidates"][0]["executable"], false);
    assert!(result.get("quote_id").is_none());
    assert!(result["excluded_sources"].as_array().unwrap().is_empty());
    body["wallet_mint_urls"] = json!([cashu.url]);
    let (status, result) = server.post("/v1/routes/compare", &body);
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(result["candidates"][0]["fee_scope"], "gateway_only");
    assert_eq!(result["candidates"][1]["fee_scope"], "cashu_reserve");
}

#[test]
fn slow_federations_do_not_delay_healthy_sources_past_the_wallet_deadline() {
    let cashu = MockMint::start("healthy");
    let healthy_fed = MockMint::start("healthy");
    let slow = (0..3)
        .map(|_| MockMint::start("fed_slow_quote"))
        .collect::<Vec<_>>();
    let mut federations = slow
        .iter()
        .enumerate()
        .map(|(index, fed)| {
            json!({
                "id": format!("fedimint:slow-{index}"), "label":"Slow federation",
                "federation_id": format!("{index:064x}"), "bridge_url":fed.url,
                "quote_backend":"local_v0121_bridge",
            })
        })
        .collect::<Vec<_>>();
    federations.push(json!({
        "id":"fedimint:healthy", "label":"Healthy federation", "federation_id":"aa".repeat(32),
        "bridge_url":healthy_fed.url, "quote_backend":"local_v0121_bridge",
    }));
    let server = ApiServer::start_with_federations(
        &json!([{"id":"cashu:source-a", "url":cashu.url}]),
        &json!([]),
        &json!([]),
        &json!(federations),
    );
    let mut body = lightning_payment(100_000);
    body.as_object_mut().unwrap().remove("candidate_connectors");
    body["strict_source_registry"] = json!(true);
    body["wallet_mint_urls"] = json!([cashu.url]);
    body["federation_connector_ids"] = json!([
        "fedimint:slow-0",
        "fedimint:slow-1",
        "fedimint:slow-2",
        "fedimint:healthy"
    ]);
    let started = std::time::Instant::now();
    let (status, result) = server.post("/v1/routes/evaluate", &body);
    assert_eq!(status, 200, "{result}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(7),
        "evaluation exceeded source budget"
    );
    let routes = std::iter::once(&result["recommended_source"])
        .chain(result["alternative_sources"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert!(routes.iter().any(|route| route["protocol"] == "cashu"));
    assert!(
        routes
            .iter()
            .any(|route| route["source_id"] == "fedimint:healthy")
    );
    let excluded = result["excluded_sources"].as_array().unwrap();
    for index in 0..3 {
        assert!(
            excluded.iter().any(
                |source| source["source_id"] == format!("fedimint:slow-{index}")
                    && source["reason"].as_str().unwrap().contains("timed out")
            ),
            "{result}"
        );
    }
}
