//! Evaluation orchestration: collect sources, quote, check feasibility, rank
//! and explain.

use ecashmesh_core::Amount;
use serde_json::json;

use crate::{
    Error, EvaluateRequest, EvaluateResponse, live,
    response::{EvidenceResponse, connector_type_code, deterministic_id, deterministic_quote_id},
    sources::{self, ConnectorBatch, Sources},
};

/// Every observed source with its evidence, for inspection. Never executable.
pub(crate) fn catalog_json(batch: &ConnectorBatch, amount: u64) -> serde_json::Value {
    json!({
        "evaluated_amount_sats": amount,
        "mode": "live",
        "read_only": true,
        "evidence": batch.connectors.iter().map(EvidenceResponse::from_connector).collect::<Vec<_>>(),
        "observations": batch.observations,
        "discovery": batch.discovery,
    })
}

#[allow(clippy::too_many_lines)] // Transport orchestration retains all structured validation/no-route branches in one boundary.
pub(crate) async fn evaluate(
    provider: &Sources,
    request: EvaluateRequest,
) -> Result<EvaluateResponse, Error> {
    request.validate()?;
    let live_destination =
        live::LiveDestination::parse(&request.destination.kind, &request.destination.value)
            .map_err(|error| {
                Error::validation(
                    "Unsupported live destination",
                    vec![format!("unsupported destination: {error}")],
                )
            })?;
    let hints = request.discovery_hints(Some(&live_destination))?;
    let registry_request = request.clone();
    let EvaluateRequest {
        amount,
        asset: _,
        destination,
        payment_intent,
        candidate_connectors,
        source_connector,
        source_mint_url,
        wallet_mint_urls,
        federation_connector_ids,
        strict_source_registry,
        ..
    } = request;

    let batch = provider.collect(Amount::from_sats(amount), hints).await?;
    let candidate_connectors = sources::resolve_ids(candidate_connectors, &batch.discovery);
    let selected = sources::select(batch.connectors.clone(), candidate_connectors)?;
    let amount = Amount::from_sats(amount);
    let destination_mint_urls = live_destination
        .destination_mint_urls()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let service = provider.cashu_service();
    let mut sources = sources::select_live_sources(
        &selected,
        source_connector,
        source_mint_url,
        wallet_mint_urls,
        federation_connector_ids,
        &batch.discovery,
        &destination_mint_urls,
        provider.allows_discovered_sources(),
        strict_source_registry,
    )?;
    // Local aliases must not silently resolve to a different federation after
    // restoring a registry on another device.
    sources.retain(|source| {
        registry_request
            .federation_identities
            .get(source.id.as_str())
            .is_none_or(|expected| {
                batch.fedimint_observations.iter().any(|observation| {
                    observation.snapshot.id == source.id
                        && observation.config.federation_id == *expected
                })
            })
    });
    let evaluation = match live::evaluate(
        service,
        provider.fedimint_service(),
        provider.liquidity(),
        provider.history(),
        &batch,
        sources,
        live_destination,
        amount,
    )
    .await
    {
        Ok(evaluation) => evaluation,
        Err(no_route) => {
            let diagnostics = source_diagnostics(
                &registry_request,
                &batch,
                &[],
                &no_route.quote_observations,
                &destination_mint_urls,
            );
            let mut error = Error::no_viable(
                "No viable live route found.",
                no_route
                    .details
                    .into_iter()
                    .chain(batch.discovery.issues.iter().map(|issue| {
                        format!("{}: {} ({})", issue.field, issue.message, issue.code)
                    }))
                    .chain(no_route.quote_observations.iter().filter_map(|quote| {
                        quote["issue"].as_object().map(|issue| {
                            format!(
                                "{}: {} ({})",
                                quote["kind"].as_str().unwrap_or("quote"),
                                issue["message"].as_str().unwrap_or("unknown quote failure"),
                                issue["code"].as_str().unwrap_or("UNKNOWN")
                            )
                        })
                    }))
                    .collect(),
            );
            error.diagnostics = Some(json!({
                "excluded_sources": diagnostics,
                "connector_observations": batch.observations,
                "quote_observations": no_route.quote_observations,
            }));
            return Err(error);
        }
    };
    let quote_id = deterministic_id(
        "live_quote",
        &[
            deterministic_quote_id(
                amount,
                &destination,
                &payment_intent,
                &evaluation.connectors,
            ),
            json!(evaluation.quote_observations).to_string(),
        ],
    );
    let mut response = EvaluateResponse::from_ranking(
        quote_id,
        &destination,
        &payment_intent,
        &evaluation.connectors,
        evaluation.ranking,
    );
    response.apply_live_fee_terms(&evaluation.fee_terms, amount);
    response.apply_destination_settlement(&destination.kind);
    response.apply_source_labels(
        &batch
            .fedimint_observations
            .iter()
            .map(|observation| {
                (
                    observation.snapshot.id.to_string(),
                    observation.config.label.clone(),
                )
            })
            .collect::<Vec<_>>(),
    );
    response.apply_fedimint_details(&batch.fedimint_observations, &evaluation.quote_observations);
    response.apply_liquidity_evidence(&evaluation.quote_observations);
    response.apply_cashu_details(&batch.live_observations, &evaluation.quote_observations);
    response.apply_reliability_evidence(&evaluation.quote_observations);
    response.apply_evidence_items();
    response.gateway_estimated_sources = evaluation.gateway_estimated_sources;
    let ranked_ids = std::iter::once(&response.recommended_source)
        .chain(&response.alternative_sources)
        .map(|route| route.source_id.clone())
        .collect::<Vec<_>>();
    response.excluded_sources = source_diagnostics(
        &registry_request,
        &batch,
        &ranked_ids,
        &evaluation.quote_observations,
        &destination_mint_urls,
    );
    response.discovery = batch.discovery;
    response.connector_observations = batch.observations;
    response.live = Some(json!({
        "quote_observations": evaluation.quote_observations,
        "graph": evaluation.graph_context,
        "execution": "evaluation_only_no_funds_moved",
    }));
    response.expires_at_unix_seconds = evaluation.expires_at_unix_seconds;
    response.expires_at = format!("unix:{}", response.expires_at_unix_seconds);
    for source in
        std::iter::once(&mut response.recommended_source).chain(&mut response.alternative_sources)
    {
        source.estimated_time_seconds = None;
    }
    response.sync_legacy_route_fields();
    Ok(response)
}

fn gateway_comparison_diagnostic(
    quote: Option<&serde_json::Value>,
    estimate: Option<&serde_json::Value>,
) -> Option<String> {
    let estimate = estimate?;
    let quote_error = quote
        .and_then(|value| value["issue"]["message"].as_str())
        .unwrap_or("Native federation fee quote is unavailable");
    if estimate["state"] == "known" {
        Some(format!(
            "Verified gateway-fee estimate is available. {quote_error}. This is not a funded, quote-backed route."
        ))
    } else {
        estimate["issue"]["message"]
            .as_str()
            .map(|error| format!("{quote_error}; gateway comparison unavailable: {error}"))
    }
}

/// The regtest-lab liquidity or routing-fee-budget reason a source was
/// excluded, if any.
fn liquidity_issue<'a>(quotes: &'a [serde_json::Value], id: &str) -> Option<&'a str> {
    quotes
        .iter()
        .filter(|value| {
            value["connector"] == id
                && matches!(
                    value["kind"].as_str(),
                    Some(
                        "lightning_liquidity"
                            | "lightning_liquidity_probe"
                            | "lightning_channel_state"
                            | "routing_fee_budget"
                    )
                )
        })
        .find_map(|value| value["issue"]["message"].as_str())
}

fn source_diagnostics(
    request: &EvaluateRequest,
    batch: &ConnectorBatch,
    ranked: &[String],
    quotes: &[serde_json::Value],
    destinations: &[String],
) -> Vec<serde_json::Value> {
    let mut identities = Vec::new();
    for raw_url in &request.wallet_mint_urls {
        let url = ecashmesh_cashu::discovery::canonical_mint_url(raw_url)
            .unwrap_or_else(|_| raw_url.clone());
        let id = batch
            .discovery
            .mints
            .iter()
            .find(|mint| mint.canonical_url == url)
            .map_or_else(|| url.clone(), |mint| mint.connector_id().to_string());
        identities.push((id, "cashu", url));
    }
    identities.extend(
        request
            .federation_connector_ids
            .iter()
            .map(|id| (id.clone(), "fedimint", String::new())),
    );
    if !request.strict_source_registry {
        identities.extend(batch.connectors.iter().map(|source| {
            (
                source.id.to_string(),
                connector_type_code(source.connector_type),
                String::new(),
            )
        }));
    }
    identities.sort();
    identities.dedup();
    identities
        .into_iter()
        .filter_map(|(id, protocol, endpoint)| {
            if ranked.contains(&id) {
                return None;
            }
            let observation = batch
                .observations
                .iter()
                .find(|value| value["connector"] == id);
            let quote = quotes.iter().find(|value| {
                value["connector"] == id && value["kind"] != "destination_mint_quote"
            });
            let gateway_estimate = quotes.iter().find(|value| {
                value["connector"] == id && value["kind"] == "fedimint_gateway_fee_estimate"
            });
            let liquidity_issue = liquidity_issue(quotes, &id);
            let reason = if destinations.contains(&endpoint) {
                "Destination is excluded from payment sources".to_owned()
            } else if request
                .federation_identities
                .get(&id)
                .is_some_and(|expected| {
                    observation.is_some_and(|value| {
                        value["federation_id"].as_str() != Some(expected.as_str())
                    })
                })
            {
                "Local connector refers to a different federation than the saved registry"
                    .to_owned()
            } else if observation.is_none() {
                "Source is not configured locally or its URL was rejected by discovery policy"
                    .to_owned()
            } else if let Some(message) = liquidity_issue {
                message.to_owned()
            } else if let Some(message) = gateway_comparison_diagnostic(quote, gateway_estimate) {
                message
            } else if let Some(message) = quote.and_then(|value| value["issue"]["message"].as_str())
            {
                message.to_owned()
            } else if quote.is_some_and(|value| value["value"]["payable"] == false) {
                "Read-only quote reports this payment is unavailable".to_owned()
            } else if let Some(message) =
                observation.and_then(|value| value["send_unavailable_reason"].as_str())
            {
                message.to_owned()
            } else {
                "No current compatible quote-backed route; inspect source and quote evidence"
                    .to_owned()
            };
            let status = if destinations.contains(&endpoint) {
                "unsupported"
            } else if observation.is_some_and(|value| value["health"]["freshness"] == "stale")
                || quote.is_some_and(|value| value["state"] == "stale")
                || reason.to_lowercase().contains("stale")
            {
                "stale"
            } else if reason.to_lowercase().contains("disabled")
                || reason.to_lowercase().contains("unsupported")
            {
                "unsupported"
            } else {
                "unavailable"
            };
            Some(
                json!({"source_id": id, "protocol": protocol, "endpoint": endpoint,
            "route_classification": status, "reason": reason,
            "evidence": observation, "quote": quote, "wallet_executable": false}),
            )
        })
        .collect()
}
