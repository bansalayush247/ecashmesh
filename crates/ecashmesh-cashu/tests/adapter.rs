use std::collections::BTreeMap;

use ecashmesh_cashu::{CashuObservation, EndpointCapture, MintCapture};
use ecashmesh_core::{
    Amount, ConnectorHealth, ConnectorId, EvidenceSource, EvidenceTimestamp, PaymentRequest,
    RouteRankingConfig, explain_ranking, rank_routes,
};
use serde_json::{Value, json};

const INFO: &str = include_str!("fixtures/info.json");
const KEYSETS: &str = include_str!("fixtures/keysets.json");
const NOW: u64 = 1_700_000_000;

fn endpoint(body: &str) -> EndpointCapture {
    EndpointCapture {
        checked_at: NOW,
        observed_at: Some(NOW),
        body: Some(body.into()),
        error: None,
        stale: false,
    }
}

fn capture() -> MintCapture {
    MintCapture {
        info: endpoint(INFO),
        keysets: endpoint(KEYSETS),
        keys: Some(endpoint(include_str!("fixtures/keys.json"))),
    }
}

fn normalize(capture: &MintCapture, now: u64) -> CashuObservation {
    capture.normalize(
        ConnectorId::new("cashu:fixture").unwrap(),
        "https://fixture.invalid/",
        now,
        300,
    )
}

fn changed_info(change: impl FnOnce(&mut Value)) -> MintCapture {
    let mut info: Value = serde_json::from_str(INFO).unwrap();
    change(&mut info);
    MintCapture {
        info: endpoint(&info.to_string()),
        ..capture()
    }
}

#[test]
fn fresh_metadata_and_schedules_do_not_invent_payment_facts() {
    let observed = normalize(&capture(), NOW);
    assert!(observed.issues.is_empty());
    let metadata = observed.metadata.observation().unwrap();
    assert_eq!(metadata.value.name.as_deref(), Some("Fixture Cashu Mint"));
    assert_eq!(metadata.source, EvidenceSource::Connector);
    assert_eq!(metadata.observed_at.unix_seconds(), NOW); // ignore remote `time`
    assert_eq!(observed.availability.value(), Some(&true));
    assert_eq!(observed.health.value(), Some(&ConnectorHealth::Healthy));
    assert_eq!(
        observed.health.observation().unwrap().source,
        EvidenceSource::Observer
    );
    assert_eq!(
        observed.melting.value().unwrap().methods[0].max_amount,
        Some(200_000)
    );
    let fees = observed.input_fees.value().unwrap();
    assert_eq!(fees.len(), 3);
    assert_eq!(
        fees.iter()
            .find(|fee| fee.id == "009a1f293253e41e")
            .unwrap()
            .input_fee_ppk,
        Some(100)
    );
    assert_eq!(fees[0].input_fee_ppk, None);
    let snapshot = observed.routing_snapshot(Amount::from_sats(100_000));
    assert!(snapshot.capabilities.can_send);
    assert!(snapshot.liquidity.is_unknown());
    assert!(snapshot.fee.is_unknown());
    assert!(snapshot.reliability.is_unknown());
    assert!(snapshot.evidence.solvency.is_unknown());
    assert_eq!(snapshot.evidence.first_observed_at, None);
}

#[test]
fn stale_and_future_timestamps_are_explicit() {
    assert!(normalize(&capture(), NOW + 300).metadata.is_known());
    let stale = normalize(&capture(), NOW + 301);
    assert!(stale.metadata.is_stale());
    assert!(stale.input_fees.is_stale());
    assert!(stale.health.is_stale());
    assert!(
        !stale
            .routing_snapshot(Amount::from_sats(100_000))
            .capabilities
            .can_send
    );
    let future = normalize(&capture(), NOW - 1);
    assert!(future.metadata.is_unknown());
    assert!(future.availability.is_unknown());
    assert!(
        future
            .issues
            .iter()
            .any(|issue| issue.code == "INVALID_TIMESTAMP")
    );
}

#[test]
fn missing_is_distinct_from_observed_unavailability() {
    let missing = EndpointCapture {
        checked_at: NOW,
        observed_at: None,
        body: None,
        error: None,
        stale: false,
    };
    let unknown = normalize(
        &MintCapture {
            info: missing.clone(),
            keysets: missing.clone(),
            keys: None,
        },
        NOW,
    );
    assert!(unknown.metadata.is_unknown());
    assert!(unknown.availability.is_unknown());
    assert!(unknown.health.is_unknown());
    let failed = EndpointCapture {
        error: Some("HTTP request timed out".into()),
        ..missing
    };
    let unavailable = normalize(
        &MintCapture {
            info: failed.clone(),
            keysets: failed,
            keys: None,
        },
        NOW,
    );
    assert_eq!(unavailable.availability.value(), Some(&false));
    assert_eq!(
        unavailable.health.value(),
        Some(&ConnectorHealth::Unavailable)
    );
    assert!(unavailable.metadata.is_unknown());
    assert!(
        unavailable
            .routing_snapshot(Amount::from_sats(100))
            .evidence
            .solvency
            .is_unknown()
    );
}

#[test]
fn refresh_failure_retains_stale_values_and_current_failure() {
    let mut capture = capture();
    capture.info.error = Some("HTTP status 503".into());
    let observed = normalize(&capture, NOW);
    assert!(observed.metadata.is_stale());
    assert!(observed.input_fees.is_known());
    assert_eq!(observed.health.value(), Some(&ConnectorHealth::Degraded));
    assert!(observed.availability.is_unknown());
    assert!(
        observed
            .issues
            .iter()
            .any(|issue| issue.code == "UNAVAILABLE")
    );
    assert!(
        observed
            .send_unavailable_reason(Amount::from_sats(100))
            .is_some()
    );
}

#[test]
fn malformed_and_partial_fields_preserve_independent_evidence() {
    let malformed = normalize(
        &MintCapture {
            info: endpoint(include_str!("fixtures/malformed.json")),
            ..capture()
        },
        NOW,
    );
    assert!(malformed.metadata.is_unknown());
    assert!(malformed.melting.is_unknown());
    assert!(malformed.input_fees.is_known());
    let partial = normalize(
        &MintCapture {
            info: endpoint(include_str!("fixtures/partial.json")),
            ..capture()
        },
        NOW,
    );
    assert_eq!(
        partial.metadata.value().unwrap().name.as_deref(),
        Some("Partial Mint")
    );
    assert_eq!(partial.metadata.value().unwrap().version, None);
    assert!(partial.minting.is_unknown());
    assert!(partial.melting.is_known());
    assert_eq!(partial.health.value(), Some(&ConnectorHealth::Degraded));
    assert_eq!(partial.issues.len(), 2);
}

#[test]
fn protocol_limits_disabled_flags_and_units_gate_send_capability() {
    let observed = normalize(&capture(), NOW);
    for amount in [100, 100_000, 200_000] {
        assert!(
            observed
                .send_unavailable_reason(Amount::from_sats(amount))
                .is_none()
        );
    }
    for amount in [99, 200_001] {
        assert!(
            observed
                .send_unavailable_reason(Amount::from_sats(amount))
                .is_some()
        );
    }
    for capture in [
        changed_info(|info| info["nuts"]["5"]["disabled"] = json!(true)),
        changed_info(|info| info["nuts"]["5"]["methods"][0]["unit"] = json!("msat")),
        changed_info(|info| info["nuts"]["5"]["methods"][0]["method"] = json!("bolt12")),
        changed_info(|info| info["nuts"]["5"]["methods"] = json!([])),
    ] {
        let observed = normalize(&capture, NOW);
        assert!(
            !observed
                .routing_snapshot(Amount::from_sats(100_000))
                .capabilities
                .can_send
        );
    }
}

#[test]
fn invalid_or_conflicting_settings_are_unknown_not_supported() {
    for capture in [
        changed_info(|info| info["nuts"]["5"]["disabled"] = json!("false")),
        changed_info(|info| info["nuts"]["5"]["methods"][0]["max_amount"] = json!(-1)),
        changed_info(|info| info["nuts"]["5"]["methods"][0]["max_amount"] = json!(1)),
        changed_info(|info| {
            let duplicate = info["nuts"]["5"]["methods"][0].clone();
            info["nuts"]["5"]["methods"]
                .as_array_mut()
                .unwrap()
                .push(duplicate);
        }),
    ] {
        let observed = normalize(&capture, NOW);
        assert!(observed.melting.is_unknown());
        assert!(
            observed
                .send_unavailable_reason(Amount::from_sats(100_000))
                .is_some()
        );
    }
}

#[test]
fn keyset_errors_do_not_become_free_payment_quotes() {
    for body in [
        "{}",
        r#"{"keysets":[{"id":"bad","unit":"sat","active":true}]}"#,
        r#"{"keysets":[{"id":"009a1f293253e41e","unit":"sat","active":true,"input_fee_ppk":-1}]}"#,
    ] {
        let observed = normalize(
            &MintCapture {
                keysets: endpoint(body),
                ..capture()
            },
            NOW,
        );
        assert!(observed.input_fees.is_unknown());
        assert!(
            observed
                .routing_snapshot(Amount::from_sats(100))
                .fee
                .is_unknown()
        );
        assert!(
            observed
                .send_unavailable_reason(Amount::from_sats(100))
                .is_some()
        );
    }
    let mut keysets: Value = serde_json::from_str(KEYSETS).unwrap();
    let duplicate = keysets["keysets"][0].clone();
    keysets["keysets"].as_array_mut().unwrap().push(duplicate);
    let conflicting = normalize(
        &MintCapture {
            keysets: endpoint(&keysets.to_string()),
            ..capture()
        },
        NOW,
    );
    assert!(conflicting.input_fees.is_unknown());
    assert!(
        conflicting
            .issues
            .iter()
            .any(|issue| issue.code == "CONFLICTING_DATA")
    );
}

#[test]
fn fixed_capture_ranks_deterministically_with_explicit_unknown_risks() {
    let observed = normalize(&capture(), NOW);
    let amount = Amount::from_sats(100_000);
    let snapshot = observed.routing_snapshot(amount);
    let evidence = BTreeMap::from([(snapshot.id.clone(), snapshot.evidence.clone())]);
    let run = || {
        rank_routes(
            PaymentRequest::new(amount),
            vec![snapshot.quote(amount).unwrap()],
            &evidence,
            EvidenceTimestamp::from_unix_seconds(NOW),
            RouteRankingConfig::default(),
        )
    };
    let ranking = run();
    assert_eq!(ranking, run());
    assert_eq!(explain_ranking(&ranking), explain_ranking(&run()));
    assert_eq!(ranking.ranked.len(), 1);
    let risks = ranking.ranked[0]
        .quality
        .risk_factors
        .iter()
        .map(ecashmesh_core::RiskFactor::reason_code)
        .collect::<Vec<_>>();
    for risk in [
        "unknown_liquidity",
        "unknown_fee",
        "unknown_reliability",
        "unknown_solvency",
    ] {
        assert!(risks.contains(&risk), "Missing risk: {risk}; {risks:?}");
    }
    let stale = normalize(&capture(), NOW + 301).routing_snapshot(amount);
    let ranking = rank_routes(
        PaymentRequest::new(amount),
        vec![stale.quote(amount).unwrap()],
        &BTreeMap::from([(stale.id.clone(), stale.evidence.clone())]),
        EvidenceTimestamp::from_unix_seconds(NOW + 301),
        RouteRankingConfig::default(),
    );
    assert!(ranking.ranked.is_empty());
    assert_eq!(ranking.rejected.len(), 1);
}

#[test]
fn expired_or_inactive_keysets_do_not_advertise_usable_send_capability() {
    for changes in [
        json!({"active":false}),
        json!({"final_expiry":NOW}),
        json!({"unit":"usd"}),
    ] {
        let mut keys: Value = serde_json::from_str(KEYSETS).unwrap();
        for (field, value) in changes.as_object().unwrap() {
            keys["keysets"][0][field] = value.clone();
        }
        let observed = normalize(
            &MintCapture {
                keysets: endpoint(&keys.to_string()),
                ..capture()
            },
            NOW,
        );
        assert!(observed.input_fees.is_known());
        assert!(
            observed
                .send_unavailable_reason(Amount::from_sats(100_000))
                .is_some()
        );
    }
}
