use std::collections::{BTreeMap, BTreeSet};

use ecashmesh_core::{ConfidenceLevel, Evidence, EvidenceSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AdapterIssue, CashuObservation, MintCapture, issue, observe, parse_endpoint, valid_keyset_id,
};

/// Public denomination information, not token material or private keys.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct PublicKeyset {
    /// Public keyset identity.
    pub id: String,
    /// Unit of these denomination amounts.
    pub unit: String,
    /// Denomination to compressed public key mapping, ordered numerically.
    pub keys: BTreeMap<u64, String>,
}

pub(super) fn extend(
    observation: &mut CashuObservation,
    capture: &MintCapture,
    info: Option<&Value>,
    now: u64,
    ttl: u64,
) {
    let issues = &mut observation.issues;
    let public_key = info.and_then(|info| info.get("pubkey")).and_then(|value| {
        if let Some(key) = value.as_str().filter(|key| valid_public_key(key)) {
            Some(key.to_ascii_lowercase())
        } else {
            issue(
                issues,
                "info.pubkey",
                "MALFORMED_DATA",
                "Expected a compressed secp256k1 public key",
            );
            None
        }
    });
    observation.public_key = observe(
        public_key,
        &capture.info,
        now,
        ttl,
        EvidenceSource::Connector,
    );
    let nuts = info
        .and_then(|info| info.get("nuts"))
        .and_then(|value| parse_nuts(value, issues));
    observation.nuts = observe(nuts, &capture.info, now, ttl, EvidenceSource::Connector);
    if let Some(capture) = &capture.keys {
        let keys = parse_endpoint(capture, "keys", now, issues)
            .and_then(|value| parse_keys(&value, issues));
        observation.public_keysets = observe(keys, capture, now, ttl, EvidenceSource::Connector);
    } else {
        issue(
            issues,
            "keys",
            "MISSING_DATA",
            "No public denomination keys observed",
        );
    }
    if let (Some(keys), Some(fees)) = (
        observation.public_keysets.value(),
        observation.input_fees.value(),
    ) && keys.iter().any(|keys| {
        fees.iter()
            .any(|fee| fee.id == keys.id && fee.unit != keys.unit)
    }) {
        issue(
            issues,
            "keys",
            "CONFLICTING_DATA",
            "Keyset unit disagrees between /v1/keys and /v1/keysets",
        );
        observation.public_keysets = Evidence::Unknown;
    }
    observation.supported_units = units(observation);
    observation
        .issues
        .sort_by(|a, b| (&a.field, &a.code, &a.message).cmp(&(&b.field, &b.code, &b.message)));
}

fn units(observation: &CashuObservation) -> Evidence<Vec<String>> {
    let lists = [
        observation.minting.clone().map(|settings| {
            settings
                .methods
                .into_iter()
                .map(|method| method.unit)
                .collect::<Vec<_>>()
        }),
        observation.melting.clone().map(|settings| {
            settings
                .methods
                .into_iter()
                .map(|method| method.unit)
                .collect()
        }),
        observation
            .input_fees
            .clone()
            .map(|fees| fees.into_iter().map(|fee| fee.unit).collect()),
    ];
    let units = lists
        .iter()
        .filter_map(Evidence::value)
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    let Some(oldest) = lists
        .iter()
        .filter_map(Evidence::observation)
        .min_by_key(|item| item.observed_at)
    else {
        return Evidence::Unknown;
    };
    let confidence = if lists.iter().any(Evidence::is_unknown) {
        ConfidenceLevel::Low
    } else {
        ConfidenceLevel::Medium
    };
    if lists.iter().any(Evidence::is_stale) {
        Evidence::reported_stale(
            units.into_iter().collect(),
            EvidenceSource::Connector,
            oldest.observed_at,
            confidence,
        )
    } else {
        Evidence::reported(
            units.into_iter().collect(),
            EvidenceSource::Connector,
            oldest.observed_at,
            confidence,
        )
    }
}

fn parse_nuts(value: &Value, issues: &mut Vec<AdapterIssue>) -> Option<BTreeMap<u16, Value>> {
    let Some(settings) = value.as_object() else {
        issue(
            issues,
            "info.nuts",
            "MALFORMED_DATA",
            "Expected a NUT settings object",
        );
        return None;
    };
    let mut nuts = BTreeMap::new();
    for (number, settings) in settings {
        let Ok(number) = number.parse::<u16>() else {
            issue(issues, "info.nuts", "MALFORMED_ENTRY", "Invalid NUT number");
            continue;
        };
        if !settings.is_object() {
            issue(
                issues,
                "info.nuts",
                "MALFORMED_ENTRY",
                "NUT settings must be an object",
            );
            continue;
        }
        if nuts.insert(number, settings.clone()).is_some() {
            issue(
                issues,
                "info.nuts",
                "CONFLICTING_DATA",
                "Duplicate normalized NUT number",
            );
            return None;
        }
    }
    if nuts.is_empty() && !settings.is_empty() {
        None
    } else {
        Some(nuts)
    }
}

fn parse_keys(value: &Value, issues: &mut Vec<AdapterIssue>) -> Option<Vec<PublicKeyset>> {
    let Some(entries) = value.get("keysets").and_then(Value::as_array) else {
        issue(
            issues,
            "keys",
            "MALFORMED_DATA",
            "Expected a public keysets array",
        );
        return None;
    };
    let mut output = Vec::new();
    for entry in entries {
        match serde_json::from_value::<PublicKeyset>(entry.clone()) {
            Ok(mut keyset)
                if valid_keyset_id(&keyset.id)
                    && !keyset.unit.is_empty()
                    && !keyset.keys.is_empty()
                    && entry
                        .get("keys")
                        .and_then(Value::as_object)
                        .is_some_and(|keys| {
                            keys.keys().all(|amount| {
                                amount
                                    .parse::<u64>()
                                    .is_ok_and(|parsed| parsed.to_string() == *amount)
                            })
                        })
                    && keyset
                        .keys
                        .iter()
                        .all(|(amount, key)| *amount > 0 && valid_public_key(key)) =>
            {
                keyset.id.make_ascii_lowercase();
                keyset
                    .keys
                    .values_mut()
                    .for_each(|key| key.make_ascii_lowercase());
                output.push(keyset);
            }
            _ => issue(
                issues,
                "keys",
                "MALFORMED_ENTRY",
                "Invalid denomination or compressed public key",
            ),
        }
    }
    output.sort_by(|a, b| a.id.cmp(&b.id));
    if output.windows(2).any(|pair| pair[0].id == pair[1].id) {
        issue(
            issues,
            "keys",
            "CONFLICTING_DATA",
            "Duplicate public keyset identifier",
        );
        return None;
    }
    if output.is_empty() && !entries.is_empty() {
        None
    } else {
        Some(output)
    }
}

fn valid_public_key(key: &str) -> bool {
    key.len() == 66
        && (key.starts_with("02") || key.starts_with("03"))
        && key.parse::<secp256k1::PublicKey>().is_ok()
}
