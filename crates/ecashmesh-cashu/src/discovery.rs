//! Bounded mint discovery and deterministic canonical identity. Directory claims
//! are hints, never independent evidence of reserves, liquidity, or reliability.

mod service;
pub use service::{DiscoveryService, DiscoveryState};

use crate::{AdapterIssue, EndpointCapture, issue};
use ecashmesh_core::ConnectorId;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Public mint auditor listing (JSON array of objects with `url`).
pub const MINT_AUDIT_DIRECTORY: &str = "https://api.audit.8333.space/mints/";
/// MVP bound applied deterministically after canonical deduplication.
pub const MAX_MINTS: usize = 64;

/// Origin of a mint URL, distinct from the mint's subsequent self-reported facts.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "type", content = "source", rename_all = "snake_case")]
pub enum DiscoverySource {
    /// Operator-configured seed list.
    Seed,
    /// Public directory endpoint.
    Directory(String),
    /// URL explicitly supplied by a host wallet.
    Wallet,
    /// Explicit mint hint on a payment request.
    PaymentRequest,
    /// Explicit mint URL attached to a destination.
    Destination,
}

/// A URL claim with provenance. Only operator configuration may create seed claims.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MintHint {
    /// Original URL as received.
    pub url: String,
    /// Optional operator-selected connector alias.
    pub alias: Option<String>,
    /// Discovery origin.
    pub source: DiscoverySource,
    /// Local capture time of this claim (not the mint's clock).
    pub observed_at: u64,
    /// A retained directory response can explicitly be stale.
    pub stale: bool,
}

/// One deduplicated mint with every distinct discovery source preserved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveredMint {
    #[serde(skip)]
    connector: ConnectorId,
    /// Stable URL-derived identifier; aliases do not change canonical identity.
    pub canonical_id: String,
    /// Normalized base URL, including any deployment subpath.
    pub canonical_url: String,
    /// Operator aliases, sorted deterministically.
    pub aliases: Vec<String>,
    /// All distinct claims including their original URL spelling and timestamps.
    pub provenance: Vec<MintHint>,
}

impl DiscoveredMint {
    fn new(url: String) -> Self {
        let canonical_id = format!("cashu:{:x}", Sha256::digest(url.as_bytes()));
        Self {
            connector: ConnectorId::new(&canonical_id).expect("SHA256 hex is a valid connector ID"),
            canonical_id,
            canonical_url: url,
            aliases: Vec::new(),
            provenance: Vec::new(),
        }
    }

    fn normalize_provenance(&mut self) {
        self.aliases.sort();
        self.aliases.dedup();
        if let Some(alias) = self
            .aliases
            .first()
            .and_then(|alias| ConnectorId::new(alias).ok())
        {
            self.connector = alias;
        }
        self.provenance.sort_by(|a, b| {
            (&a.source, &a.url, &a.alias, a.observed_at, a.stale).cmp(&(
                &b.source,
                &b.url,
                &b.alias,
                b.observed_at,
                b.stale,
            ))
        });
        self.provenance.dedup();
    }
    /// Maintains compatibility with configured connector IDs when present.
    #[must_use]
    pub fn connector_id(&self) -> ConnectorId {
        self.connector.clone()
    }
}

/// Reproducible discovery result, including rejected input diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DiscoveryReport {
    /// Canonical URL ordering, independent of source traversal order.
    pub mints: Vec<DiscoveredMint>,
    /// Malformed, stale, conflicting, or truncated input reasons.
    pub issues: Vec<AdapterIssue>,
}

/// Canonicalizes an HTTP(S) mint URL without merging HTTP/HTTPS or distinct paths.
///
/// # Errors
/// Rejects credentials, queries, fragments, ambiguous escaped separators, and oversized URLs.
pub fn canonical_mint_url(raw: &str) -> Result<String, String> {
    if raw.len() > 2048 || raw.contains('\\') || raw.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("Invalid or oversized mint URL".into());
    }
    let mut url = Url::parse(raw.trim()).map_err(|_| "Invalid mint URL")?;
    if let Some(host) = url.domain() {
        let host = host.trim_end_matches('.').to_owned();
        url.set_host(Some(&host)).map_err(|_| "Invalid hostname")?;
    }
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Mint URL must be HTTP(S) without credentials, query, or fragment".into());
    }
    let path = normalize_path(url.path())?;
    url.set_path(&format!("{}/", path.trim_end_matches('/')));
    // Reparse so decoded dot segments are normalized by the URL implementation.
    Url::parse(url.as_str())
        .map(|url| url.to_string())
        .map_err(|_| "Invalid normalized URL".into())
}

fn normalize_path(path: &str) -> Result<String, String> {
    let mut result = String::new();
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = path
                .get(index + 1..index + 3)
                .ok_or("Invalid percent escape")?;
            let byte = u8::from_str_radix(hex, 16).map_err(|_| "Invalid percent escape")?;
            if byte == b'/' || byte == b'\\' || byte.is_ascii_control() {
                return Err("Ambiguous encoded URL separator".into());
            }
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                result.push(char::from(byte));
            } else {
                result.push('%');
                result.push_str(&hex.to_ascii_uppercase());
            }
            index += 3;
        } else {
            result.push(char::from(bytes[index]));
            index += 1;
        }
    }
    Ok(result)
}

/// Deduplicates URL claims; conflicting aliases are quarantined, never silently overwritten.
#[must_use]
pub fn discover(hints: impl IntoIterator<Item = MintHint>, now: u64, ttl: u64) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let mut mints = BTreeMap::<String, DiscoveredMint>::new();
    let mut aliases = BTreeMap::<String, BTreeSet<String>>::new();
    for mut hint in hints {
        let url = match canonical_mint_url(&hint.url) {
            Ok(url) => url,
            Err(error) => {
                issue(&mut report.issues, &hint.url, "INVALID_MINT_URL", &error);
                continue;
            }
        };
        if hint.observed_at > now {
            issue(
                &mut report.issues,
                &url,
                "INVALID_TIMESTAMP",
                "Discovery timestamp is in the future",
            );
            continue;
        }
        hint.stale |= now.saturating_sub(hint.observed_at) > ttl;
        if hint.stale {
            issue(
                &mut report.issues,
                &url,
                "STALE_DISCOVERY",
                "Retained discovery claim is stale; mint data requires a fresh probe",
            );
        }
        if let Some(alias) = &hint.alias {
            if hint.source != DiscoverySource::Seed || ConnectorId::new(alias).is_err() {
                issue(
                    &mut report.issues,
                    &url,
                    "INVALID_ALIAS",
                    "Only configured seeds can define valid connector aliases",
                );
                continue;
            }
            aliases
                .entry(alias.clone())
                .or_default()
                .insert(url.clone());
        }
        let entry = mints
            .entry(url.clone())
            .or_insert_with(|| DiscoveredMint::new(url));
        if let Some(alias) = &hint.alias {
            entry.aliases.push(alias.clone());
        }
        entry.provenance.push(hint);
    }
    for mint in mints.values() {
        aliases
            .entry(mint.canonical_id.clone())
            .or_default()
            .insert(mint.canonical_url.clone());
    }
    let conflicts = aliases
        .values()
        .filter(|urls| urls.len() > 1)
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    for (url, mut mint) in mints {
        if conflicts.contains(&url) {
            issue(
                &mut report.issues,
                &url,
                "CONFLICTING_IDENTITY",
                "One configured alias refers to multiple canonical mint URLs",
            );
            continue;
        }
        mint.normalize_provenance();
        report.mints.push(mint);
    }
    if report.mints.len() > MAX_MINTS {
        report.mints.truncate(MAX_MINTS);
        issue(
            &mut report.issues,
            "discovery",
            "LIMIT_REACHED",
            "Only the first 64 canonical mints are probed",
        );
    }
    report
        .issues
        .sort_by(|a, b| (&a.field, &a.code, &a.message).cmp(&(&b.field, &b.code, &b.message)));
    report.issues.dedup();
    report
}

/// Parses a public directory response. Supports URL arrays, `{url: ...}` entries,
/// and a `{mints: [...]}` envelope. Directory metadata never overrides mint data.
#[must_use]
pub fn directory_hints(
    endpoint: &str,
    capture: &EndpointCapture,
    now: u64,
    ttl: u64,
) -> (Vec<MintHint>, Vec<AdapterIssue>) {
    let mut issues = Vec::new();
    if let Some(error) = &capture.error {
        issue(&mut issues, endpoint, "DIRECTORY_UNAVAILABLE", error);
    }
    let Some(at) = capture.observed_at.filter(|at| *at <= now) else {
        issue(
            &mut issues,
            endpoint,
            "MISSING_DATA",
            "No valid directory observation timestamp",
        );
        return (Vec::new(), issues);
    };
    let value = capture
        .body
        .as_deref()
        .and_then(|body| crate::strict_json::parse(body).ok());
    let entries = value.as_ref().and_then(|value| {
        value
            .as_array()
            .or_else(|| value.get("mints").and_then(serde_json::Value::as_array))
    });
    let Some(entries) = entries else {
        issue(
            &mut issues,
            endpoint,
            "MALFORMED_DIRECTORY",
            "Expected a JSON mint array or mints envelope",
        );
        return (Vec::new(), issues);
    };
    let mut hints = Vec::new();
    for entry in entries {
        if let Some(url) = entry
            .as_str()
            .or_else(|| entry.get("url").and_then(serde_json::Value::as_str))
        {
            hints.push(MintHint {
                url: url.into(),
                alias: None,
                source: DiscoverySource::Directory(endpoint.into()),
                observed_at: at,
                stale: capture.stale || capture.error.is_some() || now - at > ttl,
            });
        } else {
            issue(
                &mut issues,
                endpoint,
                "MALFORMED_ENTRY",
                "Directory entry has no string mint URL",
            );
        }
    }
    (hints, issues)
}
