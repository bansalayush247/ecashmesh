//! Per-route evidence inventory: every parameter the evaluation used or
//! displayed, with how it was obtained, when, and what it cannot show.
//!
//! Classification:
//! - `authoritative`: read from the system that owns the fact (guardian
//!   consensus/audit, a node's own channel table, a native quote).
//! - `measured`: an `EcashMesh` observation of behaviour (a Lightning probe,
//!   recorded real payments).
//! - `inferred`: derived from a different fact (wallet balance standing in for
//!   route liquidity).
//! - `unknown`: not obtainable here; never replaced by a number.

use serde_json::{Value, json};

use crate::EvaluatedRouteResponse;

/// Gateway health from the federation registry, the quote that selected the
/// gateway, and (in the lab) the gateway's own channel table. Health is kept
/// separate from liquidity and from payment reliability.
pub(crate) fn gateway_health(metrics: &Value, quotes: &[Value], source_id: &str) -> Value {
    let selected = &metrics["selected_gateway"];
    let registered = metrics["gateways"].as_array().map(|gateways| {
        gateways.iter().any(|gateway| {
            (selected["gateway_id"].is_string() && gateway["gateway_id"] == selected["gateway_id"])
                || (selected["gateway_url"].is_string()
                    && gateway["gateway_url"] == selected["gateway_url"])
        })
    });
    let find = |kind: &str| {
        quotes
            .iter()
            .find(|quote| quote["kind"] == kind && quote["connector"] == source_id)
    };
    let channels = find("lightning_channel_state");
    let state = channels.map(|quote| &quote["value"]);
    let bound = channels.is_some_and(|quote| quote["effect"] != "not_applied");
    let amount =
        find("lightning_liquidity").and_then(|quote| quote["value"]["amount_sats"].as_u64());
    let reachable = state.and_then(|state| state["reachable"].as_bool());
    let outbound = state.and_then(|state| state["outbound_sats"].as_u64());
    let active_channels = state.and_then(|state| state["active_channel_count"].as_u64());
    let liquidity_status = match (bound, reachable, outbound, amount) {
        (true, Some(true), Some(outbound), Some(amount)) if outbound < amount => "insufficient",
        (true, Some(true), Some(_), Some(amount))
            if state
                .and_then(|state| state["payee_direct_outbound_sats"].as_u64())
                .is_some_and(|direct| direct >= amount) =>
        {
            "sufficient_direct_channel"
        }
        (true, Some(true), Some(_), Some(_)) => "outbound_covers_amount_route_unproven",
        _ => "unknown",
    };
    let status = match (
        bound,
        reachable,
        state.and_then(|state| state["node_state"].as_str()),
    ) {
        (true, Some(true), Some("Running" | "synced")) => "healthy",
        (true, Some(true), _) => "degraded",
        (true, Some(false), _) => "unreachable",
        _ if selected["gateway_status"] == "verified" => "responding_to_quotes",
        _ => "unknown",
    };
    json!({
        "gateway_id": selected["gateway_id"],
        "gateway_url": selected["gateway_url"],
        "registered": registered,
        "protocol": selected["gateway_protocol"],
        "quote_verified": selected["gateway_status"] == "verified",
        "routing_available": selected["routing_available"],
        "status": status,
        "reachable": if bound { json!(reachable) } else { json!(null) },
        "node_state": state.filter(|_| bound).map(|state| state["node_state"].clone()),
        "synced_to_chain": state.filter(|_| bound).map(|state| state["synced_to_chain"].clone()),
        "network": state.filter(|_| bound).map(|state| state["network"].clone()),
        "active": bound.then(|| active_channels.is_some_and(|count| count > 0)),
        "channel_count": state.filter(|_| bound).map(|state| state["channel_count"].clone()),
        "active_channel_count": active_channels.filter(|_| bound),
        "outbound_liquidity_sats": outbound.filter(|_| bound),
        "inbound_liquidity_sats": state.filter(|_| bound).map(|state| state["inbound_sats"].clone()),
        "liquidity_status": liquidity_status,
        "freshness": channels.map(|quote| quote["state"].clone()),
        "observed_at_unix_seconds": state.map(|state| state["observed_at_unix_seconds"].clone()),
    })
}

#[allow(clippy::too_many_arguments)] // One flat evidence row.
fn item(
    parameter: &str,
    value: Value,
    classification: &str,
    source: &str,
    observed_at: Option<u64>,
    expires_at: Option<u64>,
    freshness: &str,
    confidence: Option<&str>,
    ranking_signal: Option<&str>,
    limitation: Option<&str>,
) -> Value {
    let mut row = json!({
        "parameter": parameter,
        "classification": classification,
        "source": source,
        "observed_at_unix_seconds": observed_at,
        "expires_at_unix_seconds": expires_at,
        "freshness": freshness,
        "confidence": confidence,
        "ranking_signal": ranking_signal,
        "limitation": limitation,
    });
    row["value"] = value;
    row
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

#[allow(clippy::too_many_lines)] // A flat inventory is easier to audit than helpers.
pub(crate) fn for_route(route: &EvaluatedRouteResponse) -> Vec<Value> {
    let mut items = Vec::new();
    let fedimint = route.fedimint_metrics.as_ref();
    let cashu = route.cashu_metrics.as_ref();
    let null = Value::Null;

    // Liquidity.
    let liquidity = route.liquidity_evidence.as_ref();
    match liquidity.and_then(|evidence| text(evidence, "basis")) {
        Some("active_probe") => {
            let probe = &liquidity.unwrap_or(&null)["probe"];
            items.push(item(
                "liquidity",
                json!(format!(
                    "{} sats routable ({})",
                    number(probe, "probed_amount_sats").unwrap_or_default(),
                    text(probe, "outcome").unwrap_or("unknown")
                )),
                "measured",
                "non-settling Lightning probe from the source's own node",
                number(probe, "observed_at_unix_seconds"),
                number(probe, "expires_at_unix_seconds"),
                text(probe, "freshness").unwrap_or("unknown"),
                text(probe, "confidence"),
                Some("liquidity_confidence"),
                Some("proves only this amount to this payee; never total capacity"),
            ));
        }
        Some("channel_state") => {
            let state = &liquidity.unwrap_or(&null)["channel_state"];
            items.push(item(
                "liquidity",
                json!(format!("{} sats active outbound, direct channel to payee {}", number(state, "outbound_sats").unwrap_or_default(), if state["payee_direct_outbound_sats"].is_u64() { "yes" } else { "no" })),
                "authoritative",
                "the source node's own channel table",
                number(state, "observed_at_unix_seconds"),
                number(state, "expires_at_unix_seconds"),
                text(state, "freshness").unwrap_or("unknown"),
                text(liquidity.unwrap_or(&null), "confidence"),
                Some("liquidity_confidence"),
                Some("balances do not check HTLC limits; sufficiency only for a direct payee channel (medium confidence)"),
            ));
        }
        _ if route.protocol == "fedimint" => items.push(item(
            "liquidity",
            json!(
                fedimint
                    .and_then(|metrics| number(metrics, "wallet_balance_sats"))
                    .map(|sats| format!("wallet balance {sats} sats"))
            ),
            "inferred",
            "lab wallet ecash balance",
            fedimint.and_then(|metrics| number(metrics, "observed_at_unix_seconds")),
            None,
            "fresh",
            Some("medium"),
            Some("liquidity_confidence"),
            Some(
                "wallet balance covers the ecash side only; the gateway's Lightning leg is unknown",
            ),
        )),
        _ => items.push(item(
            "liquidity",
            Value::Null,
            "unknown",
            "no probe or channel evidence",
            None,
            None,
            "unknown",
            None,
            Some("liquidity_confidence"),
            None,
        )),
    }
    if let Some(state) = liquidity
        .map(|evidence| &evidence["channel_state"])
        .filter(|state| state.is_object())
    {
        items.push(item(
            "channel_state",
            json!({
                "channels": state["channel_count"], "active": state["active_channel_count"],
                "outbound_sats": state["outbound_sats"], "inbound_sats": state["inbound_sats"],
                "payee_direct_outbound_sats": state["payee_direct_outbound_sats"],
            }),
            if state["reachable"] == true {
                "authoritative"
            } else {
                "unknown"
            },
            text(state, "method").unwrap_or("channel_state"),
            number(state, "observed_at_unix_seconds"),
            number(state, "expires_at_unix_seconds"),
            text(state, "freshness").unwrap_or("unknown"),
            None,
            None,
            Some("supporting evidence unless it decided liquidity"),
        ));
    }

    // Fees.
    items.push(if route.protocol == "fedimint" {
        item(
            "fee",
            json!({
                "federation_fee_msat": fedimint.map_or(&null, |m| &m["federation_fee_msat"]),
                "gateway_fee_msat": fedimint.map_or(&null, |m| &m["gateway_fee_msat"]),
                "total_fee_msat": fedimint.map_or(&null, |m| &m["total_fee_msat"]),
                "gateway_fee_ppm": fedimint.map_or(&null, |m| &m["selected_gateway"]["fee_ppm"]),
            }),
            "authoritative",
            "native non-committing LNv2 fee quote (lab wallet notes) + gateway routing fee",
            fedimint.and_then(|metrics| number(metrics, "observed_at_unix_seconds")),
            None,
            "fresh",
            Some("medium"),
            Some("fee_reasonableness"),
            None,
        )
    } else {
        item(
            "fee",
            json!({"melt_fee_reserve_sats": cashu.map_or(&null, |m| &m["melt_fee_reserve_sats"]), "input_fees_ppk": cashu.map_or(&null, |m| &m["input_fees_ppk"])}),
            "authoritative",
            "NUT-05 melt quote for this invoice",
            cashu.and_then(|metrics| number(metrics, "observed_at_unix_seconds")),
            cashu.and_then(|metrics| number(metrics, "melt_quote_expires_at_unix_seconds")),
            "fresh",
            None,
            Some("fee_reasonableness"),
            Some("fee reserve is an upper bound; NUT-02 input fees depend on the proofs selected"),
        )
    });

    // Funding.
    items.push(match fedimint {
        Some(metrics) => item(
            "funding",
            json!({
                "wallet_balance_sats": metrics["wallet_balance_sats"], "required_balance_sats": metrics["required_balance_sats"],
                "funding_headroom_sats": metrics["funding_headroom_sats"], "funding_feasible": metrics["funding_feasible"],
            }),
            "authoritative",
            text(metrics, "balance_source").unwrap_or("bridge client"),
            number(metrics, "observed_at_unix_seconds"),
            None,
            "fresh",
            None,
            None,
            Some("gates the source (exclusion); not a ranking weight"),
        ),
        None => item(
            "funding",
            Value::Null,
            "unknown",
            "wallet-held Cashu proofs",
            None,
            None,
            "unknown",
            None,
            None,
            Some("the API never sees Cashu proofs; the wallet checks its own balance"),
        ),
    });

    // Reliability and history: real payments only.
    let reliability = route.reliability_evidence.as_ref();
    let rate = reliability.and_then(|stats| number(stats, "success_rate_basis_points"));
    items.push(item(
        "reliability",
        json!({
            "success_rate_basis_points": rate,
            "counted_attempts": reliability.map_or(&null, |stats| &stats["counted_attempts"]),
            "successful_payments": reliability.map_or(&null, |stats| &stats["successful_payments"]),
            "failed_payments": reliability.map_or(&null, |stats| &stats["failed_payments"]),
        }),
        if rate.is_some() {
            "measured"
        } else {
            "unknown"
        },
        "real regtest payments recorded by the lab executor",
        reliability.and_then(|stats| number(stats, "observed_at_unix_seconds")),
        reliability.and_then(|stats| number(stats, "expires_at_unix_seconds")),
        reliability
            .and_then(|stats| text(stats, "freshness"))
            .unwrap_or("unknown"),
        reliability.and_then(|stats| text(stats, "confidence")),
        Some("reliability"),
        Some("probes, gateway discovery and fee quotes never count"),
    ));
    items.push(item(
        "historical_behavior",
        json!({
            "first_observed_at_unix_seconds": reliability.map_or(&null, |stats| &stats["first_observed_at_unix_seconds"]),
            "recorded_payment_outcomes": reliability.map_or(&null, |stats| &stats["counted_attempts"]),
        }),
        if reliability.is_some_and(|stats| stats["first_observed_at_unix_seconds"].is_u64()) { "measured" } else { "unknown" },
        "persistent lab observation history",
        reliability.and_then(|stats| number(stats, "first_observed_at_unix_seconds")),
        None,
        if reliability.is_some_and(|stats| stats["first_observed_at_unix_seconds"].is_u64()) { "fresh" } else { "unknown" },
        None,
        Some("historical_behavior"),
        Some("existing formula: midpoint(connector age / 30 days, recorded outcomes / 100)"),
    ));

    // Solvency.
    if let Some(metrics) = fedimint {
        let reserve = &metrics["reserve"];
        let status = text(reserve, "solvency_status").unwrap_or("unknown");
        let audit = &reserve["guardian_audit"];
        items.push(item(
            "solvency",
            json!({
                "status": status, "liabilities_msat": reserve["liabilities_msat"], "assets_msat": reserve["assets_msat"],
                "coverage_ratio": reserve["coverage_ratio"],
                "guardian_audit": audit.is_object().then(|| format!("{}/{} agree ({})", audit["agreeing"], audit["guardian_count"], audit["state"].as_str().unwrap_or("unknown"))),
            }),
            match status {
                "covered" | "undercovered" => "authoritative",
                _ => "unknown",
            },
            "guardian admin audit (mintv2 liabilities, walletv2 assets)",
            number(reserve, "observed_at_unix_seconds"),
            number(reserve, "expires_at_unix_seconds"),
            if matches!(status, "covered" | "undercovered") { "fresh" } else { "unknown" },
            text(reserve, "confidence"),
            Some("solvency_confidence"),
            (status == "conflicting").then_some("guardians disagreed without a threshold majority"),
        ));
        items.push(item(
            "reserve",
            json!({"reserve_sats": reserve["reserve_sats"], "pending_pegout_sats": reserve["pending_pegout_sats"], "pending_change_sats": reserve["pending_change_sats"]}),
            if reserve["reserve_sats"].is_u64() { "authoritative" } else { "unknown" },
            "walletv2 threshold consensus",
            number(metrics, "observed_at_unix_seconds"),
            None,
            if reserve["reserve_sats"].is_u64() { "fresh" } else { "unknown" },
            None,
            None,
            Some("pending peg-outs/change are not spendable reserve"),
        ));
        let health = &metrics["federation_health"];
        items.push(item(
            "federation_health",
            json!({"status": health["status"], "guardians": health["guardian_count"], "responding": health["guardians_responding"], "modules": health["modules"]}),
            if health["status"] == "healthy" { "authoritative" } else { "unknown" },
            "guardian consensus reads",
            number(health, "observed_at_unix_seconds"),
            None,
            "fresh",
            None,
            Some("reliability (health cap) and risk"),
            None,
        ));
        let gateway = &metrics["gateway_health"];
        items.push(item(
            "gateway_health",
            json!({"status": gateway["status"], "registered": gateway["registered"], "routing_available": gateway["routing_available"], "active_channels": gateway["active_channel_count"]}),
            if gateway["reachable"].is_boolean() { "authoritative" } else { "unknown" },
            "federation registry + gateway admin API",
            number(gateway, "observed_at_unix_seconds"),
            None,
            text(gateway, "freshness").unwrap_or("unknown"),
            None,
            None,
            Some("health is not liquidity and not payment success"),
        ));
    } else if let Some(metrics) = cashu {
        items.push(item(
            "solvency",
            json!({"status": "unknown"}),
            "unknown",
            "Cashu protocol",
            None,
            None,
            "unknown",
            None,
            Some("solvency_confidence"),
            text(metrics, "solvency_limitation"),
        ));
        items.push(item(
            "mint_health",
            json!({"health": metrics["mint_health"], "endpoints_reachable": metrics["endpoints_reachable"], "keysets": metrics["keyset_count"]}),
            "authoritative",
            "NUT-06/NUT-01 public endpoints",
            number(metrics, "observed_at_unix_seconds"),
            number(metrics, "expires_at_unix_seconds"),
            "fresh",
            None,
            Some("reliability (health cap) and risk"),
            Some("reachability is not payment reliability"),
        ));
    }
    items
}
