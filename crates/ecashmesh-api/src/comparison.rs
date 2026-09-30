//! Read-only fee comparison. No payment bindings or executable routes are created.
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
};
use serde_json::{Value, json};

use crate::{ApiError, AppState, EvaluateRequest, connectors::unix_now, evaluate_using};

pub(super) async fn compare(
    State(state): State<AppState>,
    request: Result<Json<EvaluateRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        evaluate_using(&state.provider, request),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            "EVALUATION_DEADLINE",
            "Route comparison exceeded its deadline. Try again.",
        )
    })?;
    // Even when normal evaluation has no funded route, verified gateway
    // estimates remain available in its diagnostics. Validation failures do
    // not turn into comparisons, and this handler never records a payment.
    let (quotes, observations, excluded) = match result {
        Ok(Json(response)) => (
            response
                .live
                .as_ref()
                .and_then(|v| v["quote_observations"].as_array())
                .cloned()
                .unwrap_or_default(),
            response.connector_observations,
            response.excluded_sources,
        ),
        Err(error) if error.code == "NO_VIABLE_ROUTE" => {
            let diagnostics = error.diagnostics.unwrap_or(Value::Null);
            (
                diagnostics["quote_observations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
                diagnostics["connector_observations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
                diagnostics["excluded_sources"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            )
        }
        Err(error) => return Err(error),
    };
    Ok(Json(rank_comparisons(
        &quotes,
        &observations,
        &excluded,
        unix_now(),
    )))
}

fn rank_comparisons(
    quotes: &[Value],
    observations: &[Value],
    excluded: &[Value],
    now: u64,
) -> Value {
    let mut candidates = Vec::new();
    for quote in quotes {
        if quote["state"] != "known" {
            continue;
        }
        let Some(source_id) = quote["connector"].as_str() else {
            continue;
        };
        let value = &quote["value"];
        let expiry = value["expiry"].as_u64();
        if expiry.is_some_and(|expiry| expiry <= now) {
            continue;
        }
        let observation = observations.iter().find(|v| v["connector"] == source_id);
        let label = observation
            .and_then(|v| v["label"].as_str())
            .or_else(|| quote["mint_url"].as_str())
            .unwrap_or(source_id);
        let balance = observation
            .and_then(|v| v.pointer("/source_balance/value/sats"))
            .and_then(Value::as_u64);
        let mut add = |fee: u64, scope: &str, gateway: Option<&Value>| {
            candidates.push(json!({
                "source_id":source_id, "source_label":label,
                "federation_id":observation.and_then(|v| v["federation_id"].as_str()),
                "gateway_id":gateway.and_then(|v| v["gateway_id"].as_str()),
                "gateway_protocol":gateway.and_then(|v| v["gateway_protocol"].as_str()),
                "fee_sats":fee, "fee_scope":scope,
                "balance_sats":balance, "balance_ignored":true,
                "executable":false, "funding_feasible":null,
                "expires_at_unix_seconds":expiry,
            }));
        };
        match quote["kind"].as_str() {
            Some("source_melt_quote") => {
                if let Some(fee) = value["fee_reserve_sats"].as_u64() {
                    add(fee, "cashu_reserve", None);
                }
            }
            Some("fedimint_lightning_fee_quote") => {
                if let Some(fee) = value["total_fee_sats"].as_u64() {
                    add(fee, "fedimint_quote", None);
                }
            }
            Some("fedimint_gateway_fee_estimate") if value["gateway_identity_verified"] == true => {
                for gateway in value["gateway_candidates"].as_array().into_iter().flatten() {
                    if let Some(fee) = gateway["gateway_fee_sats"].as_u64() {
                        add(fee, "gateway_only", Some(gateway));
                    }
                }
            }
            _ => {}
        }
    }
    candidates.sort_by(|a, b| {
        a["fee_sats"]
            .as_u64()
            .cmp(&b["fee_sats"].as_u64())
            .then_with(|| {
                (a["fee_scope"] == "gateway_only").cmp(&(b["fee_scope"] == "gateway_only"))
            })
            .then_with(|| a["source_id"].as_str().cmp(&b["source_id"].as_str()))
            .then_with(|| {
                a["gateway_protocol"]
                    .as_str()
                    .cmp(&b["gateway_protocol"].as_str())
            })
            .then_with(|| a["gateway_id"].as_str().cmp(&b["gateway_id"].as_str()))
    });
    for (index, candidate) in candidates.iter_mut().enumerate() {
        candidate["rank"] = json!(index + 1);
    }
    let unranked: Vec<_> = excluded
        .iter()
        .filter(|source| {
            !candidates
                .iter()
                .any(|c| c["source_id"] == source["source_id"])
        })
        .collect();
    json!({
        "mode":"comparison_only", "ignore_balances":true, "executable":false,
        "ranking_basis":"known_fee_ascending",
        "notice":"Ranked by observed fee only, regardless of balance. Gateway-only fees exclude federation fees; this order does not establish the cheapest total payment cost.",
        "candidates":candidates, "excluded_sources":unranked,
        "observed_at_unix_seconds":now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_ranks_zero_balance_and_partial_fees_without_execution() {
        let quotes = json!([
            {"connector":"cashu:a", "kind":"source_melt_quote", "state":"known", "value":{"fee_reserve_sats":10,"expiry":130}},
            {"connector":"fedimint:b", "kind":"fedimint_gateway_fee_estimate", "state":"known", "value":{"gateway_identity_verified":true,"expiry":130,"gateway_candidates":[{"gateway_id":"g","gateway_protocol":"lnv2","gateway_fee_sats":7}]}},
            {"connector":"cashu:expired", "kind":"source_melt_quote", "state":"known", "value":{"fee_reserve_sats":0,"expiry":99}}
        ]);
        let observations = json!([{"connector":"fedimint:b","federation_id":"fed-b","source_balance":{"value":{"sats":0}}}]);
        let result = rank_comparisons(
            quotes.as_array().unwrap(),
            observations.as_array().unwrap(),
            &[],
            100,
        );
        assert_eq!(result["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(result["candidates"][0]["source_id"], "fedimint:b");
        assert_eq!(result["candidates"][0]["balance_sats"], 0);
        assert_eq!(result["candidates"][0]["fee_scope"], "gateway_only");
        assert_eq!(result["candidates"][0]["executable"], false);
        assert!(result["candidates"][0]["funding_feasible"].is_null());
        assert!(result.get("quote_id").is_none());
    }
}
