//! Provider selection and DTO translation. Protocol parsing lives in ecashmesh-cashu.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ecashmesh_cashu::{
    CashuObservation,
    discovery::{DiscoveryReport, DiscoveryService, DiscoverySource, MintHint},
};
use ecashmesh_core::{
    Amount, ConnectorId, ConnectorSnapshot, DEMO_EVALUATED_AT, EvidenceTimestamp,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, EvidenceStateResponse, connector_health_code};

#[path = "payment_mode.rs"]
mod payment_mode;

#[derive(Clone)]
pub(super) enum Provider {
    Simulator,
    Cashu(Arc<DiscoveryService>),
}

pub(super) struct ConnectorBatch {
    pub connectors: Vec<ConnectorSnapshot>,
    /// Protocol observations retained for live quote-backed route construction.
    pub live_observations: Vec<CashuObservation>,
    pub observations: Vec<Value>,
    pub evaluated_at: EvidenceTimestamp,
    pub expires_at_unix_seconds: u64,
    pub simulated: bool,
    pub discovery: DiscoveryReport,
}

#[derive(Deserialize)]
struct ConfigInput {
    id: Option<String>,
    url: String,
}

pub(super) fn resolve_ids(ids: Vec<String>, discovery: &DiscoveryReport) -> Vec<String> {
    ids.into_iter()
        .map(|id| {
            if let Some(mint) = discovery
                .mints
                .iter()
                .find(|mint| mint.canonical_id == id || mint.aliases.contains(&id))
            {
                mint.canonical_id.clone()
            } else {
                id
            }
        })
        .collect()
}
