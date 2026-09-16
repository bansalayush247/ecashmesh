//! Cashu discovery and unpaid-quote transport.
//!
//! Transaction limits are not liquidity. Input fees are not a melt fee quote.
//! HTTP reachability is not payment reliability or proof of reserves.

pub mod discovery;
mod metadata;
mod payment_request;
mod strict_json;
mod transport;
pub use metadata::PublicKeyset;
pub use payment_request::{
    CashuPaymentRequest, CashuPaymentRequestEncoding, CashuPaymentRequestError,
    CashuSupportedMethod, CashuTransport,
};
pub use transport::{CashuAdapter, MeltQuote, MintConfig, MintQuote, QuoteObservation};

use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence, ConnectorHealth,
    ConnectorId, ConnectorSnapshot, ConnectorType, Evidence, EvidenceSource, EvidenceTimestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Captured GET result. Caller timestamps, never the mint's self-reported clock,
/// determine freshness. Fixtures can be replayed with an explicit evaluation time.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EndpointCapture {
    /// Timestamp of the most recent fetch attempt.
    pub checked_at: u64,
    /// Timestamp of the retained response, if any.
    pub observed_at: Option<u64>,
    /// Raw JSON response, retained for deterministic normalization.
    pub body: Option<String>,
    /// Current transport failure, even when an older response was retained.
    pub error: Option<String>,
    /// Forces stale state for retained data independently of its timestamp.
    #[serde(default)]
    pub stale: bool,
}

/// Both public endpoint captures for a mint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MintCapture {
    /// GET /v1/info.
    pub info: EndpointCapture,
    /// GET /v1/keysets; only keyset descriptors, not keys or proofs.
    pub keysets: EndpointCapture,
    /// GET /v1/keys; optional for replaying older captures.
    #[serde(default)]
    pub keys: Option<EndpointCapture>,
}

/// Optional, self-reported mint display information.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct MintMetadata {
    /// Human-readable mint name.
    pub name: Option<String>,
    /// Mint implementation/version string.
    pub version: Option<String>,
    /// Short mint description.
    pub description: Option<String>,
    /// Mint message to users.
    pub motd: Option<String>,
}

/// An advertised method/unit pair and its transaction limits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaymentMethod {
    /// Cashu method, e.g. bolt11.
    pub method: String,
    /// Protocol unit, e.g. sat. Never silently convert other units to sats.
    pub unit: String,
    /// Minimum transaction amount; not a liquidity measurement.
    pub min_amount: Option<u64>,
    /// Maximum transaction amount; not a liquidity measurement.
    pub max_amount: Option<u64>,
}

/// Advertised NUT-04 or NUT-05 settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MethodSettings {
    /// Whether the mint explicitly disables this operation.
    pub disabled: bool,
    /// Supported method/unit pairs.
    pub methods: Vec<PaymentMethod>,
}

/// Public keyset input fee schedule, not the total fee for a payment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeysetFee {
    /// Public keyset identifier.
    pub id: String,
    /// Fee unit.
    pub unit: String,
    /// Whether the mint permits new outputs from the keyset.
    pub active: bool,
    /// Parts per thousand per input. None means the NUT-02 default of zero.
    pub input_fee_ppk: Option<u64>,
    /// Optional keyset final expiry.
    pub final_expiry: Option<u64>,
}

/// Structured, ordered diagnostic explaining unusable or incomplete data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdapterIssue {
    /// Endpoint or field associated with this issue.
    pub field: String,
    /// Stable machine-readable reason.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

fn issue(issues: &mut Vec<AdapterIssue>, field: &str, code: &str, message: &str) {
    issues.push(AdapterIssue {
        field: field.into(),
        code: code.into(),
        message: message.into(),
    });
}

/// Normalized observations, retaining provenance and explicit missing/stale states.
#[derive(Clone, Debug)]
pub struct CashuObservation {
    /// Configured stable identifier; never taken from a remote payload.
    pub id: ConnectorId,
    /// Configured mint URL, used as endpoint provenance.
    pub mint_url: String,
    /// Observation/evaluation time.
    pub evaluated_at: EvidenceTimestamp,
    /// Earliest expiry of the public responses used by this observation.
    pub expires_at_unix_seconds: u64,
    /// Display information reported by /v1/info.
    pub metadata: Evidence<MintMetadata>,
    /// NUT-06 mint identity public key, not a custody key.
    pub public_key: Evidence<String>,
    /// All advertised NUT settings, including optional and future NUTs.
    pub nuts: Evidence<std::collections::BTreeMap<u16, Value>>,
    /// Public denomination keys, grouped by keyset and unit.
    pub public_keysets: Evidence<Vec<PublicKeyset>>,
    /// Union of reported method/keyset units. Partial when any underlying list is absent.
    pub supported_units: Evidence<Vec<String>>,
    /// Receive capabilities reported in NUT-04.
    pub minting: Evidence<MethodSettings>,
    /// Send capabilities and liquidity-related transaction limits from NUT-05.
    pub melting: Evidence<MethodSettings>,
    /// NUT-02 input fee schedules; missing fees default to zero only per keyset.
    pub input_fees: Evidence<Vec<KeysetFee>>,
    /// Reachability of both public HTTP endpoints, not payment availability.
    pub availability: Evidence<bool>,
    /// Health of the metadata read, not payment reliability.
    pub health: Evidence<ConnectorHealth>,
    /// Stable ordered diagnostics, including missing fields and refresh failures.
    pub issues: Vec<AdapterIssue>,
}

impl MintCapture {
    /// Normalizes captured public responses without network access or wall clocks.
    #[must_use]
    pub fn normalize(
        &self,
        id: ConnectorId,
        mint_url: &str,
        now: u64,
        max_age_seconds: u64,
    ) -> CashuObservation {
        let mut issues = Vec::new();
        let info = parse_endpoint(&self.info, "info", now, &mut issues);
        let keysets = parse_endpoint(&self.keysets, "keysets", now, &mut issues);
        let metadata = info
            .as_ref()
            .map(|value| parse_metadata(value, &mut issues));
        let minting = settings(info.as_ref(), "4", &mut issues);
        let melting = settings(info.as_ref(), "5", &mut issues);
        let fees = keysets
            .as_ref()
            .and_then(|value| parse_fees(value, &mut issues));
        let reachable = self.info.error.is_none()
            && self.info.body.is_some()
            && self.keysets.error.is_none()
            && self.keysets.body.is_some();
        let both_failed = self.info.error.is_some() && self.keysets.error.is_some();
        let health = if both_failed {
            ConnectorHealth::Unavailable
        } else if reachable && issues.is_empty() {
            ConnectorHealth::Healthy
        } else {
            ConnectorHealth::Degraded
        };
        // Health uses the last attempt, while each retained value keeps its original age.
        let checked_at = self.info.checked_at.min(self.keysets.checked_at);
        let status_capture = EndpointCapture {
            checked_at,
            observed_at: Some(checked_at),
            body: None,
            error: None,
            stale: false,
        };
        let status_known = checked_at <= now
            && [&self.info, &self.keysets]
                .iter()
                .any(|capture| capture.body.is_some() || capture.error.is_some());
        let availability = if both_failed || reachable {
            observe(
                status_known.then_some(reachable),
                &status_capture,
                now,
                max_age_seconds,
                EvidenceSource::Observer,
            )
        } else {
            Evidence::Unknown
        };
        let mut observation = CashuObservation {
            id,
            mint_url: mint_url.into(),
            evaluated_at: EvidenceTimestamp::from_unix_seconds(now),
            expires_at_unix_seconds: [self.info.observed_at, self.keysets.observed_at]
                .into_iter()
                .map(|at| at.map_or(now, |at| at.saturating_add(max_age_seconds)))
                .min()
                .unwrap_or(now),
            metadata: observe(
                metadata,
                &self.info,
                now,
                max_age_seconds,
                EvidenceSource::Connector,
            ),
            public_key: Evidence::Unknown,
            nuts: Evidence::Unknown,
            public_keysets: Evidence::Unknown,
            supported_units: Evidence::Unknown,
            minting: observe(
                minting,
                &self.info,
                now,
                max_age_seconds,
                EvidenceSource::Connector,
            ),
            melting: observe(
                melting,
                &self.info,
                now,
                max_age_seconds,
                EvidenceSource::Connector,
            ),
            input_fees: observe(
                fees,
                &self.keysets,
                now,
                max_age_seconds,
                EvidenceSource::Connector,
            ),
            availability,
            health: observe(
                status_known.then_some(health),
                &status_capture,
                now,
                max_age_seconds,
                EvidenceSource::Observer,
            ),
            issues,
        };
        metadata::extend(&mut observation, self, info.as_ref(), now, max_age_seconds);
        observation
    }
}

impl CashuObservation {
    /// Explains why this adapter cannot advertise a send capability for this amount.
    /// This only checks advertised Cashu support; it does not rank routes or assert liquidity.
    #[must_use]
    pub fn send_unavailable_reason(&self, amount: Amount) -> Option<&'static str> {
        if !matches!(self.availability, Evidence::Known(ref observed) if observed.value) {
            return Some("Public endpoint availability is unknown, stale, or unavailable");
        }
        let Evidence::Known(settings) = &self.melting else {
            return Some("Cashu send capabilities are missing, malformed, or stale");
        };
        if settings.value.disabled {
            return Some("The mint reports melting disabled");
        }
        if !matches!(&self.input_fees, Evidence::Known(fees) if fees.value.iter().any(|keyset|
            keyset.active && keyset.unit == "sat" && keyset.final_expiry.is_none_or(|expiry|
                expiry > self.evaluated_at.unix_seconds())))
        {
            return Some("No fresh active sat keyset is available");
        }
        if !settings.value.methods.iter().any(|method| {
            method.method == "bolt11"
                && method.unit == "sat"
                && method.min_amount.is_none_or(|min| amount.sats() >= min)
                && method.max_amount.is_none_or(|max| amount.sats() <= max)
        }) {
            return Some("No advertised bolt11/sat method supports this payment amount");
        }
        None
    }

    /// Translates protocol facts into core inputs. All unmeasured payment facts stay unknown.
    #[must_use]
    pub fn routing_snapshot(&self, amount: Amount) -> ConnectorSnapshot {
        let can_send = self.send_unavailable_reason(amount).is_none();
        let can_receive = matches!(&self.minting, Evidence::Known(settings)
            if !settings.value.disabled && settings.value.methods.iter()
                .any(|method| method.method == "bolt11" && method.unit == "sat"));
        ConnectorSnapshot {
            id: self.id.clone(),
            connector_type: ConnectorType::Cashu,
            capabilities: ConnectorCapabilities::new(
                can_send,
                can_receive,
                false,
                can_send || can_receive,
            ),
            liquidity: Evidence::Unknown,
            fee: Evidence::Unknown,
            reliability: Evidence::Unknown,
            evidence: ConnectorEvidence::new(
                self.id.clone(),
                None,
                self.health.clone(),
                Evidence::Unknown,
                Evidence::Unknown,
            ),
        }
    }
}

fn observe<T>(
    value: Option<T>,
    capture: &EndpointCapture,
    now: u64,
    ttl: u64,
    source: EvidenceSource,
) -> Evidence<T> {
    let (Some(value), Some(at)) = (value, capture.observed_at) else {
        return Evidence::Unknown;
    };
    if at > now {
        return Evidence::Unknown;
    }
    let at_timestamp = EvidenceTimestamp::from_unix_seconds(at);
    if capture.stale || capture.error.is_some() || now - at > ttl {
        Evidence::reported_stale(value, source, at_timestamp, ConfidenceLevel::Low)
    } else {
        Evidence::reported(value, source, at_timestamp, ConfidenceLevel::Medium)
    }
}

fn parse_endpoint(
    capture: &EndpointCapture,
    field: &str,
    now: u64,
    issues: &mut Vec<AdapterIssue>,
) -> Option<Value> {
    if let Some(error) = &capture.error {
        issue(issues, field, "UNAVAILABLE", error);
    }
    let Some(body) = &capture.body else {
        issue(issues, field, "MISSING_DATA", "No response body available");
        return None;
    };
    if capture.observed_at.is_none_or(|at| at > now) || capture.checked_at > now {
        issue(
            issues,
            field,
            "INVALID_TIMESTAMP",
            "Missing or future observation timestamp",
        );
        return None;
    }
    if let Ok(value @ Value::Object(_)) = strict_json::parse(body) {
        Some(value)
    } else {
        issue(
            issues,
            field,
            "MALFORMED_DATA",
            "Expected a valid JSON object",
        );
        None
    }
}

fn parse_metadata(value: &Value, issues: &mut Vec<AdapterIssue>) -> MintMetadata {
    let mut metadata = MintMetadata::default();
    for (name, output) in [
        ("name", &mut metadata.name),
        ("version", &mut metadata.version),
        ("description", &mut metadata.description),
        ("motd", &mut metadata.motd),
    ] {
        match value.get(name) {
            Some(Value::String(text)) => *output = Some(text.clone()),
            None | Some(Value::Null) => {}
            _ => issue(issues, name, "MALFORMED_FIELD", "Expected a string"),
        }
    }
    metadata
}

fn settings(
    info: Option<&Value>,
    nut: &str,
    issues: &mut Vec<AdapterIssue>,
) -> Option<MethodSettings> {
    let field = format!("info.nuts.{nut}");
    let Some(value) = info
        .and_then(|info| info.get("nuts"))
        .and_then(|nuts| nuts.get(nut))
    else {
        issue(
            issues,
            &field,
            "MISSING_DATA",
            "Method settings were not reported",
        );
        return None;
    };
    match serde_json::from_value::<MethodSettings>(value.clone()) {
        Ok(mut settings) if settings.methods.iter().all(|method| {
            !method.method.is_empty() && !method.unit.is_empty()
                && !matches!((method.min_amount, method.max_amount), (Some(min), Some(max)) if min > max)
        }) => {
            settings.methods.sort_by(|a, b| (&a.method, &a.unit, a.min_amount, a.max_amount)
                .cmp(&(&b.method, &b.unit, b.min_amount, b.max_amount)));
            if settings.methods.windows(2).any(|pair| pair[0].method == pair[1].method && pair[0].unit == pair[1].unit) {
                issue(issues, &field, "CONFLICTING_DATA", "Duplicate method/unit settings");
                None
            } else { Some(settings) }
        }
        _ => {
            issue(issues, &field, "MALFORMED_DATA", "Invalid method settings or amount limits");
            None
        }
    }
}

fn parse_fees(value: &Value, issues: &mut Vec<AdapterIssue>) -> Option<Vec<KeysetFee>> {
    let Some(entries) = value.get("keysets").and_then(Value::as_array) else {
        issue(issues, "keysets", "MALFORMED_DATA", "Missing keysets array");
        return None;
    };
    let mut fees = Vec::new();
    for entry in entries {
        match serde_json::from_value::<KeysetFee>(entry.clone()) {
            Ok(mut fee) if valid_keyset_id(&fee.id) && !fee.unit.is_empty() => {
                fee.id.make_ascii_lowercase();
                fees.push(fee);
            }
            _ => issue(
                issues,
                "keysets",
                "MALFORMED_ENTRY",
                "Invalid keyset fee descriptor",
            ),
        }
    }
    fees.sort_by(|a, b| a.id.cmp(&b.id));
    if fees.windows(2).any(|pair| pair[0].id == pair[1].id) {
        issue(
            issues,
            "keysets",
            "CONFLICTING_DATA",
            "Duplicate keyset identifiers",
        );
        return None;
    }
    if fees.is_empty() && !entries.is_empty() {
        None
    } else {
        Some(fees)
    }
}

fn valid_keyset_id(id: &str) -> bool {
    ((id.len() == 16 && id.starts_with("00")) || (id.len() == 66 && id.starts_with("01")))
        && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}
