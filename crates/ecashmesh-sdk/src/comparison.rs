//! Read-only fee comparison. No payment bindings or executable routes are created.
use serde_json::{Value, json};

use crate::{Error, EvaluateResponse, sources::unix_now};

/// Fee-only comparison built from an evaluation's evidence. Even when no
/// source is fundable, verified gateway estimates remain in the no-route
/// diagnostics; validation failures never become comparisons.
pub(crate) fn compare(result: Result<EvaluateResponse, Error>) -> Result<Value, Error> {
    let (quotes, observations, excluded) = match result {
        Ok(response) => (
            response
                .live
                .as_ref()
                .and_then(|v| v["quote_observations"].as_array())
                .cloned()
                .unwrap_or_default(),
            response.connector_observations,
            response.excluded_sources,
        ),
        Err(error) if error.kind == crate::ErrorKind::NoViableRoute => {
            let diagnostics = error.diagnostics.unwrap_or(Value::Null);
            let list = |key: &str| diagnostics[key].as_array().cloned().unwrap_or_default();
            (
                list("quote_observations"),
                list("connector_observations"),
                list("excluded_sources"),
            )
        }
        Err(error) => return Err(error),
    };
    Ok(rank_comparisons(
        &quotes,
        &observations,
        &excluded,
        unix_now(),
    ))
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
