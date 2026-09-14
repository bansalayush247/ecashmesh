use ecashmesh_cashu::{EndpointCapture, MintCapture};
use ecashmesh_core::{ConnectorId, EvidenceSource};
use serde_json::{Value, json};

fn endpoint(body: &str) -> EndpointCapture {
    EndpointCapture {
        checked_at: 1000,
        observed_at: Some(1000),
        body: Some(body.into()),
        error: None,
        stale: false,
    }
}
fn capture() -> MintCapture {
    MintCapture {
        info: endpoint(include_str!("fixtures/info.json")),
        keysets: endpoint(include_str!("fixtures/keysets.json")),
        keys: Some(endpoint(include_str!("fixtures/keys.json"))),
    }
}
fn normalize(capture: &MintCapture, now: u64) -> ecashmesh_cashu::CashuObservation {
    capture.normalize(
        ConnectorId::new("cashu:test").unwrap(),
        "https://mint.example/",
        now,
        300,
    )
}

#[test]
fn complete_public_information_is_normalized_with_provenance() {
    let observed = normalize(&capture(), 1000);
    assert!(observed.issues.is_empty(), "{:?}", observed.issues);
    assert_eq!(observed.public_key.value().unwrap().len(), 66);
    assert_eq!(
        observed.public_key.observation().unwrap().source,
        EvidenceSource::Connector
    );
    assert!(observed.nuts.value().unwrap().contains_key(&999));
    assert_eq!(observed.supported_units.value().unwrap(), &["sat", "usd"]);
    let keysets = observed.public_keysets.value().unwrap();
    assert_eq!(keysets[0].keys.keys().copied().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(keysets[0].unit, "sat");
    assert_eq!(
        observed
            .public_keysets
            .observed_at()
            .unwrap()
            .unix_seconds(),
        1000
    );
}

#[test]
fn malformed_public_keys_and_denominations_remain_unknown_without_losing_other_fields() {
    for bad_key in [
        "not a key".to_owned(),
        format!("02{}", "ff".repeat(32)),
        "☃".repeat(22),
    ] {
        let mut capture = capture();
        let mut info: Value = serde_json::from_str(capture.info.body.as_ref().unwrap()).unwrap();
        info["pubkey"] = json!(bad_key);
        capture.info.body = Some(info.to_string());
        let observed = normalize(&capture, 1000);
        assert!(observed.public_key.is_unknown());
        assert!(observed.metadata.is_known());
        assert!(observed.public_keysets.is_known());
    }
    for amount in ["0", "-1", "18446744073709551616", "bad", "01"] {
        let mut capture = capture();
        let mut keys: Value =
            serde_json::from_str(capture.keys.as_ref().unwrap().body.as_ref().unwrap()).unwrap();
        let value = keys["keysets"][0]["keys"]["1"].clone();
        keys["keysets"][0]["keys"] = json!({amount: value});
        capture.keys = Some(endpoint(&keys.to_string()));
        let observed = normalize(&capture, 1000);
        assert!(observed.public_keysets.is_unknown(), "{amount}");
        assert!(observed.input_fees.is_known());
    }
}

#[test]
fn duplicate_json_members_do_not_silently_override_evidence() {
    let mut capture = capture();
    capture.info.body =
        Some(r#"{"nuts":{"5":{"disabled":true,"disabled":false,"methods":[]}}}"#.into());
    let observed = normalize(&capture, 1000);
    assert!(observed.melting.is_unknown());
    assert!(observed.nuts.is_unknown());
    assert!(
        observed
            .issues
            .iter()
            .any(|issue| issue.code == "MALFORMED_DATA")
    );
}

#[test]
fn conflicting_endpoint_units_and_duplicate_keysets_are_not_silently_merged() {
    let mut capture = capture();
    let mut keys: Value =
        serde_json::from_str(capture.keys.as_ref().unwrap().body.as_ref().unwrap()).unwrap();
    keys["keysets"][0]["unit"] = json!("usd");
    capture.keys = Some(endpoint(&keys.to_string()));
    let observed = normalize(&capture, 1000);
    assert!(observed.public_keysets.is_unknown());
    assert!(
        observed
            .issues
            .iter()
            .any(|issue| issue.code == "CONFLICTING_DATA")
    );
    let duplicate = keys["keysets"][0].clone();
    keys["keysets"].as_array_mut().unwrap().push(duplicate);
    capture.keys = Some(endpoint(&keys.to_string()));
    assert!(normalize(&capture, 1000).public_keysets.is_unknown());
}

#[test]
fn stale_missing_and_partial_new_metadata_remain_explicit() {
    let stale = normalize(&capture(), 1301);
    assert!(stale.public_key.is_stale());
    assert!(stale.nuts.is_stale());
    assert!(stale.public_keysets.is_stale());
    assert!(stale.supported_units.is_stale());
    let mut partial = capture();
    partial.keys = None;
    let observed = normalize(&partial, 1000);
    assert!(observed.public_keysets.is_unknown());
    assert!(observed.public_key.is_known());
    assert!(observed.issues.iter().any(|issue| issue.field == "keys"));
}
