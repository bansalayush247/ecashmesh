//! Compact, snapshot-based routing primitives for large connector populations.
//!
//! This module deliberately separates the slow control plane (discovery,
//! registry updates, and graph compilation) from the route-query data plane.
//! Route queries only read an immutable [`GraphSnapshot`].  They never wait for
//! protocol discovery, registry mutation, or topology rebuild work.
//!
//! The graph contains only adapter-declared, executable transfer edges.  It is
//! not a mint directory and it does not manufacture mint-to-mint paths.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque, btree_map::Entry},
    fmt,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    time::{Duration, Instant},
};

use arc_swap::ArcSwap;

use crate::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence, ConnectorId,
    ConnectorSnapshot, ConnectorType, EvidenceTimestamp, PaymentRequest, RouteCandidate, RouteHop,
    RouteRanking, RouteRankingConfig, rank_routes,
};

const BASIS_POINTS: u16 = 10_000;
const MAX_COMPACT_CONNECTORS: usize = u32::MAX as usize;

fn saturating_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn saturating_u16(value: u128) -> u16 {
    u16::try_from(value.min(u128::from(u16::MAX))).unwrap_or(u16::MAX)
}

fn compact_id_from_index(index: usize) -> CompactConnectorId {
    match u32::try_from(index) {
        Ok(index) => CompactConnectorId::from_index(index),
        Err(_) => unreachable!("graph size is checked before compact-ID conversion"),
    }
}

fn edge_id_from_index(index: usize) -> EdgeId {
    match u32::try_from(index) {
        Ok(index) => EdgeId::from_index(index),
        Err(_) => unreachable!("graph size is checked before compact-ID conversion"),
    }
}

/// A dense connector identifier used in the routing hot path.
///
/// It is an index into a graph snapshot's node array.  External string IDs stay
/// in the control-plane registry and are only resolved after cheap search has
/// narrowed a query to a small number of paths.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompactConnectorId(u32);

impl CompactConnectorId {
    /// Creates an ID from its dense zero-based index.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    /// Returns the dense zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A dense edge identifier used for overlay lookup.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EdgeId(u32);

impl EdgeId {
    /// Creates an ID from its dense zero-based index.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    /// Returns the dense zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Explicit state of an observation used during cheap routing.
///
/// `Conflicting` is intentionally distinct from both `Unknown` and negative
/// evidence.  A conflict means sources disagree; it never means an endpoint is
/// usable or has a particular liquidity level.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EvidenceState {
    /// A current observation supports the field.
    Known,
    /// No usable observation was supplied.
    Unknown,
    /// An observation exists but is older than its freshness policy allows.
    Stale,
    /// Multiple observations disagree and no value was selected as truth.
    Conflicting,
}

impl EvidenceState {
    const fn uncertainty_cost(self) -> u16 {
        match self {
            Self::Known => 0,
            Self::Stale => 2,
            Self::Unknown => 4,
            Self::Conflicting => 6,
        }
    }
}

/// A capacity observation for one requested amount band or executable edge.
///
/// `amount` is an adapter-declared executable bound, not inferred mint
/// liquidity or solvency.  An absent amount is explicit: callers must not
/// synthesize a limit from metadata, denomination lists, or transaction limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmountAwareEvidence {
    /// Whether the observation is known, unknown, stale, or conflicting.
    pub state: EvidenceState,
    /// Adapter-provided amount bound, if one was actually observed.
    pub amount: Option<Amount>,
    /// Observation time, if supplied by the adapter.
    pub observed_at: Option<EvidenceTimestamp>,
    /// Confidence in the observation source.
    pub confidence: ConfidenceLevel,
}

impl AmountAwareEvidence {
    /// Creates explicit known evidence for an executable amount bound.
    #[must_use]
    pub const fn known(
        amount: Amount,
        observed_at: EvidenceTimestamp,
        confidence: ConfidenceLevel,
    ) -> Self {
        Self {
            state: EvidenceState::Known,
            amount: Some(amount),
            observed_at: Some(observed_at),
            confidence,
        }
    }

    /// Creates explicit stale evidence for an executable amount bound.
    #[must_use]
    pub const fn stale(
        amount: Amount,
        observed_at: EvidenceTimestamp,
        confidence: ConfidenceLevel,
    ) -> Self {
        Self {
            state: EvidenceState::Stale,
            amount: Some(amount),
            observed_at: Some(observed_at),
            confidence,
        }
    }

    /// Creates missing evidence without implying a capacity bound.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            state: EvidenceState::Unknown,
            amount: None,
            observed_at: None,
            confidence: ConfidenceLevel::None,
        }
    }

    /// Creates conflicting evidence without selecting an asserted amount.
    #[must_use]
    pub const fn conflicting(observed_at: Option<EvidenceTimestamp>) -> Self {
        Self {
            state: EvidenceState::Conflicting,
            amount: None,
            observed_at,
            confidence: ConfidenceLevel::None,
        }
    }
}

/// Adapter-normalized health state for a connector.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HealthState {
    /// The connector was observed to be available.
    Healthy,
    /// The connector was observed available with reduced service quality.
    Degraded,
    /// The connector was observed unavailable.
    Unavailable,
    /// No health assertion is available.
    Unknown,
}

/// Current health evidence tracked separately from liquidity and solvency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HealthObservation {
    /// Observation state.
    pub state: EvidenceState,
    /// The observed health value.
    pub health: HealthState,
    /// When it was observed, if known.
    pub observed_at: Option<EvidenceTimestamp>,
}

impl HealthObservation {
    /// Creates a current health observation.
    #[must_use]
    pub const fn known(health: HealthState, observed_at: EvidenceTimestamp) -> Self {
        Self {
            state: EvidenceState::Known,
            health,
            observed_at: Some(observed_at),
        }
    }

    /// Creates a missing health observation.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            state: EvidenceState::Unknown,
            health: HealthState::Unknown,
            observed_at: None,
        }
    }

    fn definitely_unavailable(self) -> bool {
        self.state == EvidenceState::Known && self.health == HealthState::Unavailable
    }

    const fn uncertainty_cost(self) -> u16 {
        let health_cost = match self.health {
            HealthState::Healthy => 0,
            HealthState::Degraded => 2,
            HealthState::Unavailable => 8,
            HealthState::Unknown => 4,
        };
        health_cost + self.state.uncertainty_cost()
    }
}

/// A transferable protocol mechanism explicitly declared by adapters.
///
/// Edges using this type are the only edges accepted by [`GraphBuilder`].
/// There is no fall-through rule that connects arbitrary mints because they
/// happen to share a protocol family.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TransferMechanism {
    /// An adapter can execute a Cashu-to-Lightning transfer.
    CashuLightning,
    /// An adapter can execute a Fedimint-to-Lightning transfer.
    FedimintLightning,
    /// An adapter can execute a declared federation gateway transfer.
    FedimintGateway,
    /// An adapter has declared a specific, verified cross-connector bridge.
    VerifiedBridge,
    /// An adapter has declared a direct execution path.
    Direct,
}

/// Compact capability categories indexed by a graph snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    /// Connector can originate a payment or transfer.
    Send,
    /// Connector can accept a payment or transfer.
    Receive,
    /// Connector supports an explicit cross-connector transfer mechanism.
    CrossConnectorTransfer,
    /// Connector can transfer through Lightning.
    Lightning,
}

const SEND_BIT: u8 = 1;
const RECEIVE_BIT: u8 = 1 << 1;
const CROSS_CONNECTOR_BIT: u8 = 1 << 2;
const LIGHTNING_BIT: u8 = 1 << 3;

const fn capability_bits(capabilities: ConnectorCapabilities) -> u8 {
    (if capabilities.can_send { SEND_BIT } else { 0 })
        | (if capabilities.can_receive {
            RECEIVE_BIT
        } else {
            0
        })
        | (if capabilities.supports_cross_connector_transfer {
            CROSS_CONNECTOR_BIT
        } else {
            0
        })
        | (if capabilities.supports_lightning {
            LIGHTNING_BIT
        } else {
            0
        })
}

const fn has_capability(bits: u8, capability: Capability) -> bool {
    let required = match capability {
        Capability::Send => SEND_BIT,
        Capability::Receive => RECEIVE_BIT,
        Capability::CrossConnectorTransfer => CROSS_CONNECTOR_BIT,
        Capability::Lightning => LIGHTNING_BIT,
    };
    bits & required != 0
}

/// Full control-plane connector data, outside graph adjacency structures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredConnector {
    /// Stable dense ID assigned by the registry.
    pub compact_id: CompactConnectorId,
    /// External protocol-agnostic ID.
    pub connector_id: ConnectorId,
    /// Protocol family.
    pub connector_type: ConnectorType,
    /// Adapter-reported capabilities.
    pub capabilities: ConnectorCapabilities,
    /// Last health observation.
    pub health: HealthObservation,
    /// Adapter-provided routing facts used only after cheap search.
    pub snapshot: ConnectorSnapshot,
    /// Discovery generation containing the latest update.
    pub discovery_generation: u64,
}

/// A discovery update accepted by the connector registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredConnector {
    /// Normalized connector facts from a protocol adapter.
    pub snapshot: ConnectorSnapshot,
    /// Health observation obtained with the snapshot.
    pub health: HealthObservation,
}

/// Control-plane state which assigns compact IDs and applies incremental updates.
#[derive(Clone, Debug, Default)]
pub struct ConnectorRegistry {
    by_external_id: BTreeMap<ConnectorId, CompactConnectorId>,
    connectors: Vec<RegisteredConnector>,
    generation: u64,
}

/// Immutable registry data pinned to a graph generation for detailed evaluation.
#[derive(Clone, Debug)]
pub struct ConnectorRegistrySnapshot {
    generation: u64,
    connectors: Arc<[RegisteredConnector]>,
}

impl ConnectorRegistrySnapshot {
    /// Returns the registry generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Resolves compact data after a route search has already selected a path.
    #[must_use]
    pub fn get(&self, id: CompactConnectorId) -> Option<&RegisteredConnector> {
        self.connectors.get(id.index())
    }

    /// Returns the number of registered connectors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.connectors.len()
    }

    /// Returns whether no connectors are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.connectors.is_empty()
    }
}

/// Summary of an applied discovery batch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegistryUpdateReport {
    /// Newly assigned compact IDs.
    pub inserted: usize,
    /// Existing connector records replaced by fresher adapter data.
    pub updated: usize,
    /// Registry generation after the update.
    pub generation: u64,
}

/// Registry mutation errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    /// A snapshot's outer and embedded connector IDs differ.
    IdentityMismatch,
    /// Dense u32 connector IDs are exhausted.
    CapacityExceeded,
    /// One batch contained more than one observation for the same identity.
    DuplicateUpdate { id: ConnectorId },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityMismatch => formatter.write_str("connector snapshot identity mismatch"),
            Self::CapacityExceeded => {
                formatter.write_str("maximum compact connector capacity exceeded")
            }
            Self::DuplicateUpdate { id } => write!(formatter, "duplicate connector update: {id}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl ConnectorRegistry {
    /// Applies a batch atomically from the perspective of subsequent snapshots.
    ///
    /// The batch is sorted by external ID before IDs are assigned, making first
    /// discovery deterministic even when an adapter completes requests in a
    /// different order. Existing compact IDs are never reused.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::CapacityExceeded`] when the dense `u32` ID
    /// space is exhausted, [`RegistryError::IdentityMismatch`] if internal
    /// registry data no longer agrees with its stable external ID map, or
    /// [`RegistryError::DuplicateUpdate`] for ambiguous same-batch updates.
    pub fn apply_incremental(
        &mut self,
        updates: impl IntoIterator<Item = DiscoveredConnector>,
    ) -> Result<RegistryUpdateReport, RegistryError> {
        let mut ordered = updates.into_iter().collect::<Vec<_>>();
        ordered.sort_unstable_by(|left, right| left.snapshot.id.cmp(&right.snapshot.id));
        if let Some(duplicate) = ordered
            .windows(2)
            .find(|pair| pair[0].snapshot.id == pair[1].snapshot.id)
        {
            return Err(RegistryError::DuplicateUpdate {
                id: duplicate[0].snapshot.id.clone(),
            });
        }

        let mut report = RegistryUpdateReport::default();
        self.generation = self.generation.saturating_add(1);
        for update in ordered {
            let id = update.snapshot.id.clone();
            if let Some(compact_id) = self.by_external_id.get(&id).copied() {
                let Some(record) = self.connectors.get_mut(compact_id.index()) else {
                    return Err(RegistryError::IdentityMismatch);
                };
                if record.connector_id != id {
                    return Err(RegistryError::IdentityMismatch);
                }
                record.connector_type = update.snapshot.connector_type;
                record.capabilities = update.snapshot.capabilities;
                record.health = update.health;
                record.snapshot = update.snapshot;
                record.discovery_generation = self.generation;
                report.updated += 1;
                continue;
            }
            if self.connectors.len() >= MAX_COMPACT_CONNECTORS {
                return Err(RegistryError::CapacityExceeded);
            }
            let compact_id = compact_id_from_index(self.connectors.len());
            let record = RegisteredConnector {
                compact_id,
                connector_id: id.clone(),
                connector_type: update.snapshot.connector_type,
                capabilities: update.snapshot.capabilities,
                health: update.health,
                snapshot: update.snapshot,
                discovery_generation: self.generation,
            };
            self.by_external_id.insert(id, compact_id);
            self.connectors.push(record);
            report.inserted += 1;
        }
        report.generation = self.generation;
        Ok(report)
    }

    /// Resolves an external connector ID in the control plane.
    #[must_use]
    pub fn resolve(&self, id: &ConnectorId) -> Option<CompactConnectorId> {
        self.by_external_id.get(id).copied()
    }

    /// Produces immutable detail data for a graph publication.
    #[must_use]
    pub fn snapshot(&self) -> ConnectorRegistrySnapshot {
        ConnectorRegistrySnapshot {
            generation: self.generation,
            connectors: Arc::from(self.connectors.clone()),
        }
    }
}

/// Compact node data traversed by the search engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphNode {
    capability_bits: u8,
    connector_type: ConnectorType,
    health: HealthObservation,
}

impl GraphNode {
    /// Creates compact node data from adapter capabilities and health evidence.
    #[must_use]
    pub const fn new(
        connector_type: ConnectorType,
        capabilities: ConnectorCapabilities,
        health: HealthObservation,
    ) -> Self {
        Self {
            capability_bits: capability_bits(capabilities),
            connector_type,
            health,
        }
    }

    /// Returns the protocol family for diagnostics and detail resolution.
    #[must_use]
    pub const fn connector_type(self) -> ConnectorType {
        self.connector_type
    }

    /// Returns whether the node supports a compact capability class.
    #[must_use]
    pub const fn supports(self, capability: Capability) -> bool {
        has_capability(self.capability_bits, capability)
    }

    /// Returns health evidence attached to this snapshot.
    #[must_use]
    pub const fn health(self) -> HealthObservation {
        self.health
    }
}

/// Four bytes of node data are sufficient for the search path. Full health
/// timestamps and connector metadata remain in `ConnectorRegistrySnapshot`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompactGraphNode {
    capability_bits: u8,
    health_state: EvidenceState,
    health: HealthState,
}

impl From<GraphNode> for CompactGraphNode {
    fn from(node: GraphNode) -> Self {
        Self {
            capability_bits: node.capability_bits,
            health_state: node.health.state,
            health: node.health.health,
        }
    }
}

impl CompactGraphNode {
    const fn supports(self, capability: Capability) -> bool {
        has_capability(self.capability_bits, capability)
    }

    const fn health(self) -> HealthObservation {
        HealthObservation {
            state: self.health_state,
            health: self.health,
            observed_at: None,
        }
    }

    fn definitely_unavailable_for_search(self) -> bool {
        self.health().definitely_unavailable()
    }
}

/// An executable edge submitted to the graph compiler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutableEdge {
    /// Origin connector.
    pub from: CompactConnectorId,
    /// Destination connector.
    pub to: CompactConnectorId,
    /// The actual adapter-declared transfer mechanism.
    pub mechanism: TransferMechanism,
    /// Fixed fee observed for this execution path.
    pub base_fee: Amount,
    /// Proportional fee in parts per million of the requested amount.
    pub fee_parts_per_million: u32,
    /// Explicit amount-bound evidence for this executable edge.
    pub amount_evidence: AmountAwareEvidence,
    /// State of edge-level execution/reliability evidence.
    pub execution_evidence: EvidenceState,
}

impl ExecutableEdge {
    /// Computes an overflow-safe fee estimate for a payment amount.
    #[must_use]
    pub fn estimated_fee(self, amount: Amount) -> Amount {
        let variable =
            (u128::from(amount.sats()) * u128::from(self.fee_parts_per_million)) / 1_000_000;
        let total = u128::from(self.base_fee.sats()).saturating_add(variable);
        Amount::from_sats(saturating_u64(total))
    }
}

/// One sparse overlay replacement for a base edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EdgeOverlay {
    /// Edge modified by this overlay entry.
    pub edge_id: EdgeId,
    /// Whether execution is currently disabled.
    pub disabled: bool,
    /// New amount-bound evidence, if runtime observations changed.
    pub amount_evidence: Option<AmountAwareEvidence>,
    /// New execution-evidence state, if runtime observations changed.
    pub execution_evidence: Option<EvidenceState>,
}

/// Immutable sparse dynamic state layered over topology.
#[derive(Clone, Debug, Default)]
pub struct GraphOverlay {
    version: u64,
    entries: Arc<[EdgeOverlay]>,
}

impl GraphOverlay {
    /// Creates a deterministic overlay. Last update for an edge wins.
    #[must_use]
    pub fn new(version: u64, updates: impl IntoIterator<Item = EdgeOverlay>) -> Self {
        let mut entries = updates.into_iter().collect::<Vec<_>>();
        entries.sort_unstable_by_key(|entry| entry.edge_id);
        let mut deduplicated = Vec::with_capacity(entries.len());
        for entry in entries {
            if deduplicated
                .last()
                .is_some_and(|last: &EdgeOverlay| last.edge_id == entry.edge_id)
            {
                if let Some(last) = deduplicated.last_mut() {
                    *last = entry;
                }
            } else {
                deduplicated.push(entry);
            }
        }
        Self {
            version,
            entries: Arc::from(deduplicated),
        }
    }

    /// Returns the overlay version.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    fn get(&self, edge_id: EdgeId) -> Option<EdgeOverlay> {
        self.entries
            .binary_search_by_key(&edge_id, |entry| entry.edge_id)
            .ok()
            .map(|index| self.entries[index])
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompactAmountEvidence {
    state: EvidenceState,
    has_amount: bool,
    amount_sats: u64,
}

impl From<AmountAwareEvidence> for CompactAmountEvidence {
    fn from(evidence: AmountAwareEvidence) -> Self {
        Self {
            state: evidence.state,
            has_amount: evidence.amount.is_some(),
            amount_sats: evidence.amount.map_or(0, Amount::sats),
        }
    }
}

impl CompactAmountEvidence {
    fn definitely_insufficient(self, requested: Amount) -> bool {
        matches!(self.state, EvidenceState::Known | EvidenceState::Stale)
            && self.has_amount
            && self.amount_sats < requested.sats()
    }
}

/// Hot-path edge representation. Timestamps, source provenance, and confidence
/// stay in the registry because detailed evaluation reads only surviving paths.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompactEdge {
    base_fee_sats: u64,
    amount_limit_sats: u64,
    to: CompactConnectorId,
    fee_parts_per_million: u32,
    mechanism: TransferMechanism,
    amount_state: EvidenceState,
    execution_evidence: EvidenceState,
    has_amount_limit: bool,
}

impl From<ExecutableEdge> for CompactEdge {
    fn from(edge: ExecutableEdge) -> Self {
        Self {
            base_fee_sats: edge.base_fee.sats(),
            amount_limit_sats: edge.amount_evidence.amount.map_or(0, Amount::sats),
            to: edge.to,
            fee_parts_per_million: edge.fee_parts_per_million,
            mechanism: edge.mechanism,
            amount_state: edge.amount_evidence.state,
            execution_evidence: edge.execution_evidence,
            has_amount_limit: edge.amount_evidence.amount.is_some(),
        }
    }
}

impl CompactEdge {
    const fn amount_evidence(self) -> CompactAmountEvidence {
        CompactAmountEvidence {
            state: self.amount_state,
            has_amount: self.has_amount_limit,
            amount_sats: self.amount_limit_sats,
        }
    }

    fn estimated_fee(self, amount: Amount) -> Amount {
        let variable =
            (u128::from(amount.sats()) * u128::from(self.fee_parts_per_million)) / 1_000_000;
        let total = u128::from(self.base_fee_sats) + variable;
        Amount::from_sats(saturating_u64(total))
    }
}

/// Capability-to-node index stored as sorted dense ID arrays.
#[derive(Clone, Debug, Default)]
struct CapabilityIndexes {
    send: Arc<[CompactConnectorId]>,
    receive: Arc<[CompactConnectorId]>,
    cross_connector: Arc<[CompactConnectorId]>,
    lightning: Arc<[CompactConnectorId]>,
}

impl CapabilityIndexes {
    fn build(nodes: &[CompactGraphNode]) -> Self {
        let mut send = Vec::new();
        let mut receive = Vec::new();
        let mut cross_connector = Vec::new();
        let mut lightning = Vec::new();
        for (index, node) in nodes.iter().copied().enumerate() {
            let id = compact_id_from_index(index);
            if node.supports(Capability::Send) {
                send.push(id);
            }
            if node.supports(Capability::Receive) {
                receive.push(id);
            }
            if node.supports(Capability::CrossConnectorTransfer) {
                cross_connector.push(id);
            }
            if node.supports(Capability::Lightning) {
                lightning.push(id);
            }
        }
        Self {
            send: Arc::from(send),
            receive: Arc::from(receive),
            cross_connector: Arc::from(cross_connector),
            lightning: Arc::from(lightning),
        }
    }

    fn get(&self, capability: Capability) -> &[CompactConnectorId] {
        match capability {
            Capability::Send => &self.send,
            Capability::Receive => &self.receive,
            Capability::CrossConnectorTransfer => &self.cross_connector,
            Capability::Lightning => &self.lightning,
        }
    }
}

/// Immutable compact adjacency topology shared by snapshots.
#[derive(Clone, Debug)]
struct GraphTopology {
    nodes: Arc<[CompactGraphNode]>,
    /// CSR row offsets; `offsets[node]..offsets[node + 1]` is its adjacency.
    offsets: Arc<[u32]>,
    edges: Arc<[CompactEdge]>,
    capability_indexes: CapabilityIndexes,
    estimated_memory_bytes: usize,
}

/// A published, immutable graph view used by every route query.
#[derive(Clone, Debug)]
pub struct GraphSnapshot {
    topology: Arc<GraphTopology>,
    overlay: Arc<GraphOverlay>,
    version: u64,
    registry_generation: u64,
}

impl GraphSnapshot {
    /// Returns the monotonically increasing graph version used by cache keys.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Returns the registry generation which supplied detail records.
    #[must_use]
    pub const fn registry_generation(&self) -> u64 {
        self.registry_generation
    }

    /// Returns the number of compact nodes.
    #[must_use]
    pub fn connector_count(&self) -> usize {
        self.topology.nodes.len()
    }

    /// Returns the number of declared executable edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.topology.edges.len()
    }

    /// Returns a structural memory estimate without allocator overhead.
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        self.topology.estimated_memory_bytes
    }

    /// Publishes a new immutable dynamic overlay without rebuilding adjacency.
    #[must_use]
    pub fn with_overlay(&self, version: u64, overlay: GraphOverlay) -> Self {
        Self {
            topology: Arc::clone(&self.topology),
            overlay: Arc::new(overlay),
            version,
            registry_generation: self.registry_generation,
        }
    }

    fn node(&self, id: CompactConnectorId) -> Option<CompactGraphNode> {
        self.topology.nodes.get(id.index()).copied()
    }

    fn edge_range(&self, from: CompactConnectorId) -> Option<(usize, usize)> {
        let index = from.index();
        let start = *self.topology.offsets.get(index)? as usize;
        let end = *self.topology.offsets.get(index + 1)? as usize;
        Some((start, end))
    }

    fn resolved_edge(&self, edge_id: EdgeId) -> Option<ResolvedEdge> {
        let base = *self.topology.edges.get(edge_id.index())?;
        let overlay = self.overlay.get(edge_id);
        Some(ResolvedEdge {
            base,
            disabled: overlay.is_some_and(|entry| entry.disabled),
            amount_evidence: overlay
                .and_then(|entry| entry.amount_evidence)
                .map_or_else(|| base.amount_evidence(), Into::into),
            execution_evidence: overlay
                .and_then(|entry| entry.execution_evidence)
                .unwrap_or(base.execution_evidence),
        })
    }

    fn indexed(&self, capability: Capability) -> &[CompactConnectorId] {
        self.topology.capability_indexes.get(capability)
    }
}

#[derive(Clone, Copy, Debug)]
struct ResolvedEdge {
    base: CompactEdge,
    disabled: bool,
    amount_evidence: CompactAmountEvidence,
    execution_evidence: EvidenceState,
}

/// Graph compilation errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphBuildError {
    /// Graph node count cannot fit the compact u32 ID space.
    TooManyConnectors,
    /// Edge count cannot fit the compact u32 edge ID space.
    TooManyEdges,
    /// An edge refers to an absent source or destination node.
    UnknownConnector {
        from: CompactConnectorId,
        to: CompactConnectorId,
    },
}

impl fmt::Display for GraphBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyConnectors => {
                formatter.write_str("too many connectors for compact graph IDs")
            }
            Self::TooManyEdges => formatter.write_str("too many edges for compact graph IDs"),
            Self::UnknownConnector { from, to } => write!(
                formatter,
                "edge {from:?}->{to:?} refers to an unknown connector"
            ),
        }
    }
}

impl std::error::Error for GraphBuildError {}

/// Builds cache-friendly CSR graph snapshots off the route-query path.
#[derive(Clone, Debug)]
pub struct GraphBuilder {
    nodes: Vec<GraphNode>,
    edges: Vec<ExecutableEdge>,
    registry_generation: u64,
}

impl GraphBuilder {
    /// Starts a compiler input with dense nodes ordered by compact ID.
    #[must_use]
    pub fn new(nodes: Vec<GraphNode>) -> Self {
        Self {
            nodes,
            edges: Vec::new(),
            registry_generation: 0,
        }
    }

    /// Associates the graph with a registry generation for detailed lookup.
    #[must_use]
    pub const fn registry_generation(mut self, generation: u64) -> Self {
        self.registry_generation = generation;
        self
    }

    /// Adds one adapter-declared executable edge.
    pub fn add_edge(&mut self, edge: ExecutableEdge) {
        self.edges.push(edge);
    }

    /// Adds multiple adapter-declared executable edges.
    pub fn extend_edges(&mut self, edges: impl IntoIterator<Item = ExecutableEdge>) {
        self.edges.extend(edges);
    }

    /// Compiles a deterministic immutable snapshot with an empty overlay.
    ///
    /// # Errors
    ///
    /// Returns an error when a graph exceeds compact-ID capacity or an edge
    /// names a source or destination which is not present in the node array.
    pub fn build(mut self, version: u64) -> Result<GraphSnapshot, GraphBuildError> {
        if self.nodes.len() > MAX_COMPACT_CONNECTORS {
            return Err(GraphBuildError::TooManyConnectors);
        }
        if self.edges.len() > MAX_COMPACT_CONNECTORS {
            return Err(GraphBuildError::TooManyEdges);
        }
        let node_count = self.nodes.len();
        for edge in &self.edges {
            if edge.from.index() >= node_count || edge.to.index() >= node_count {
                return Err(GraphBuildError::UnknownConnector {
                    from: edge.from,
                    to: edge.to,
                });
            }
        }
        self.edges.sort_unstable_by(|left, right| {
            (
                left.from,
                left.to,
                left.mechanism,
                left.base_fee,
                left.fee_parts_per_million,
                left.amount_evidence.state,
                left.amount_evidence.amount,
                left.execution_evidence,
            )
                .cmp(&(
                    right.from,
                    right.to,
                    right.mechanism,
                    right.base_fee,
                    right.fee_parts_per_million,
                    right.amount_evidence.state,
                    right.amount_evidence.amount,
                    right.execution_evidence,
                ))
        });
        let mut offsets = vec![0_u32; node_count + 1];
        for edge in &self.edges {
            offsets[edge.from.index() + 1] = offsets[edge.from.index() + 1].saturating_add(1);
        }
        for index in 1..offsets.len() {
            offsets[index] = offsets[index].saturating_add(offsets[index - 1]);
        }
        let compact_nodes = self.nodes.into_iter().map(Into::into).collect::<Vec<_>>();
        let compact_edges = self.edges.into_iter().map(Into::into).collect::<Vec<_>>();
        let capability_indexes = CapabilityIndexes::build(&compact_nodes);
        let estimated_memory_bytes = compact_nodes.len() * std::mem::size_of::<CompactGraphNode>()
            + offsets.len() * std::mem::size_of::<u32>()
            + compact_edges.len() * std::mem::size_of::<CompactEdge>()
            + capability_indexes.send.len() * std::mem::size_of::<CompactConnectorId>()
            + capability_indexes.receive.len() * std::mem::size_of::<CompactConnectorId>()
            + capability_indexes.cross_connector.len() * std::mem::size_of::<CompactConnectorId>()
            + capability_indexes.lightning.len() * std::mem::size_of::<CompactConnectorId>();
        Ok(GraphSnapshot {
            topology: Arc::new(GraphTopology {
                nodes: Arc::from(compact_nodes),
                offsets: Arc::from(offsets),
                edges: Arc::from(compact_edges),
                capability_indexes,
                estimated_memory_bytes,
            }),
            overlay: Arc::new(GraphOverlay::default()),
            version,
            registry_generation: self.registry_generation,
        })
    }
}

/// Lock-free snapshot publication for route queries.
///
/// Writers build a complete graph or overlay off-path, then replace one `Arc`.
/// Readers clone the current `Arc` without a graph write lock and continue on a
/// stable view even while discovery publishes a newer version.
#[derive(Debug)]
pub struct GraphSnapshotPublisher {
    current: ArcSwap<GraphSnapshot>,
}

impl GraphSnapshotPublisher {
    /// Creates a publisher with an initial graph snapshot.
    #[must_use]
    pub fn new(initial: Arc<GraphSnapshot>) -> Self {
        Self {
            current: ArcSwap::from(initial),
        }
    }

    /// Reads the latest immutable graph snapshot.
    #[must_use]
    pub fn load(&self) -> Arc<GraphSnapshot> {
        self.current.load_full()
    }

    /// Publishes a fully compiled snapshot atomically.
    pub fn publish(&self, snapshot: Arc<GraphSnapshot>) {
        self.current.store(snapshot);
    }
}

/// Selects an endpoint without scanning every connector at query time.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SearchEndpoint {
    /// A caller already knows the compact connector ID.
    Connector(CompactConnectorId),
    /// Use the precomputed index for connectors with this capability.
    Capability(Capability),
}

/// A bounded route-search request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteSearchRequest {
    /// Payment amount used for edge feasibility and fee estimates.
    pub amount: Amount,
    /// Known source connectors or indexed source capabilities.
    pub sources: Vec<SearchEndpoint>,
    /// Known destination connectors or indexed target capabilities.
    pub destinations: Vec<SearchEndpoint>,
    /// Maximum recommended results returned after detailed evaluation.
    pub top_k: usize,
}

impl RouteSearchRequest {
    /// Creates a bounded request with no endpoint assumptions.
    #[must_use]
    pub const fn new(amount: Amount) -> Self {
        Self {
            amount,
            sources: Vec::new(),
            destinations: Vec::new(),
            top_k: 3,
        }
    }
}

/// Tunable limits for cheap graph traversal and admission control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteSearchConfig {
    /// Maximum number of executable graph edges in a candidate path.
    pub maximum_hops: u8,
    /// Maximum nodes materialized from each capability index per endpoint set.
    /// This prevents a broad capability (for example, Lightning support) from
    /// becoming a full connector scan at query time.
    pub maximum_indexed_endpoints: usize,
    /// Hard bound for dequeued labels/nodes per query.
    pub maximum_nodes_explored: usize,
    /// Hard bound for inspected edges per query.
    pub maximum_edges_explored: usize,
    /// Maximum non-dominated labels retained per node.
    pub maximum_pareto_labels_per_node: usize,
    /// Candidate routes produced by cheap search before detailed evaluation.
    pub cheap_candidate_limit: usize,
    /// Maximum concurrent route searches admitted locally.
    pub maximum_in_flight_searches: usize,
    /// Maximum locally admitted searches per rate window.
    pub maximum_requests_per_window: usize,
    /// Rate-limit window size in seconds.
    pub rate_window_seconds: u64,
    /// Rough fee-equivalent cost of one uncertainty point during cheap ordering.
    pub uncertainty_penalty_sats: u64,
}

impl RouteSearchConfig {
    /// Conservative bounded defaults suitable for an in-process router.
    #[must_use]
    pub const fn production_default() -> Self {
        Self {
            maximum_hops: 4,
            maximum_indexed_endpoints: 256,
            maximum_nodes_explored: 4_096,
            maximum_edges_explored: 32_768,
            maximum_pareto_labels_per_node: 4,
            cheap_candidate_limit: 24,
            maximum_in_flight_searches: 64,
            maximum_requests_per_window: 4_000,
            rate_window_seconds: 1,
            uncertainty_penalty_sats: 25,
        }
    }

    fn normalized(self) -> Self {
        Self {
            maximum_hops: self.maximum_hops.max(1),
            maximum_indexed_endpoints: self.maximum_indexed_endpoints.max(1),
            maximum_nodes_explored: self.maximum_nodes_explored.max(1),
            maximum_edges_explored: self.maximum_edges_explored.max(1),
            maximum_pareto_labels_per_node: self.maximum_pareto_labels_per_node.max(1),
            cheap_candidate_limit: self.cheap_candidate_limit.max(1),
            maximum_in_flight_searches: self.maximum_in_flight_searches.max(1),
            maximum_requests_per_window: self.maximum_requests_per_window.max(1),
            rate_window_seconds: self.rate_window_seconds.max(1),
            uncertainty_penalty_sats: self.uncertainty_penalty_sats,
        }
    }
}

impl Default for RouteSearchConfig {
    fn default() -> Self {
        Self::production_default()
    }
}

/// One route surviving cheap feasibility search, before detailed evidence work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactRoute {
    /// Dense node path. Adjacent pairs are adapter-declared executable edges.
    pub path: Vec<CompactConnectorId>,
    /// Exact sparse edge path. This preserves distinct declared mechanisms
    /// between the same pair of connectors without inventing an equivalence.
    pub edge_ids: Vec<EdgeId>,
    /// Cheap fee estimate from graph edge data.
    pub estimated_fee: Amount,
    /// Number of traversed graph edges.
    pub hop_count: u8,
    /// Count of stale, unknown, or conflicting fields encountered during search.
    pub uncertainty: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SearchCost {
    fee: Amount,
    hops: u8,
    uncertainty: u16,
}

impl SearchCost {
    const fn zero() -> Self {
        Self {
            fee: Amount::ZERO,
            hops: 0,
            uncertainty: 0,
        }
    }

    fn extended(self, fee: Amount, uncertainty: u16) -> Self {
        Self {
            fee: self
                .fee
                .checked_add(fee)
                .unwrap_or(Amount::from_sats(u64::MAX)),
            hops: self.hops.saturating_add(1),
            uncertainty: self.uncertainty.saturating_add(uncertainty),
        }
    }

    fn dominates(self, other: Self) -> bool {
        self.fee <= other.fee
            && self.hops <= other.hops
            && self.uncertainty <= other.uncertainty
            && (self.fee < other.fee
                || self.hops < other.hops
                || self.uncertainty < other.uncertainty)
    }

    fn priority(self, config: RouteSearchConfig) -> u64 {
        self.fee
            .sats()
            .saturating_add(
                u64::from(self.uncertainty).saturating_mul(config.uncertainty_penalty_sats),
            )
            .saturating_add(u64::from(self.hops))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SearchLabel {
    at: CompactConnectorId,
    cost: SearchCost,
    path: Vec<CompactConnectorId>,
    edge_ids: Vec<EdgeId>,
}

/// `BinaryHeap` is a max heap, so this ordering reverses the lowest cost first.
impl Ord for SearchLabel {
    fn cmp(&self, other: &Self) -> Ordering {
        // The config-independent queue order is intentionally only a cheap
        // expansion preference. The result is sorted by full deterministic cost.
        (
            other.cost.fee,
            other.cost.hops,
            other.cost.uncertainty,
            &other.path,
            &other.edge_ids,
        )
            .cmp(&(
                self.cost.fee,
                self.cost.hops,
                self.cost.uncertainty,
                &self.path,
                &self.edge_ids,
            ))
    }
}

impl PartialOrd for SearchLabel {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Counters from one bounded graph-search attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RouteSearchMetrics {
    /// Source and destination pairs materialized through capability indexes.
    pub candidates_generated: u64,
    /// Labels removed from the priority queue.
    pub nodes_explored: u64,
    /// Compact adjacency entries inspected.
    pub edges_explored: u64,
    /// Labels dropped due to dominance or per-node Pareto bounds.
    pub pareto_pruned: u64,
    /// Edges rejected as impossible before scoring.
    pub feasibility_pruned: u64,
    /// Whether a versioned cheap-search cache entry was used.
    pub cache_hit: bool,
    /// Search wall-clock latency. It is sampled only after route work finishes.
    pub latency: Duration,
}

/// Output of the two-stage scalable routing flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteSearchResult {
    /// Graph version pinned for this decision.
    pub graph_version: u64,
    /// Routes that survived graph feasibility/candidate generation.
    pub cheap_candidates: Vec<CompactRoute>,
    /// Detailed, explainable results for the small survivor set.
    pub ranking: RouteRanking,
    /// Bounded-search and cache counters.
    pub metrics: RouteSearchMetrics,
}

/// Reasons a search cannot be admitted or resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteSearchError {
    /// The in-process bounded-concurrency limit was reached.
    Backpressured,
    /// The local per-window request rate was reached.
    RateLimited,
    /// A request did not resolve at least one source and destination.
    EmptyEndpointSet,
    /// Detailed records do not correspond to the graph being searched.
    RegistryGenerationMismatch { graph: u64, registry: u64 },
}

impl fmt::Display for RouteSearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backpressured => formatter.write_str("route search is backpressured"),
            Self::RateLimited => formatter.write_str("route search is rate limited"),
            Self::EmptyEndpointSet => {
                formatter.write_str("route search resolved no usable endpoints")
            }
            Self::RegistryGenerationMismatch { graph, registry } => write!(
                formatter,
                "graph generation {graph} does not match registry generation {registry}"
            ),
        }
    }
}

impl std::error::Error for RouteSearchError {}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RouteCacheKey {
    graph_version: u64,
    amount: Amount,
    sources: Vec<SearchEndpoint>,
    destinations: Vec<SearchEndpoint>,
    maximum_hops: u8,
    maximum_indexed_endpoints: usize,
    maximum_nodes_explored: usize,
    maximum_edges_explored: usize,
    maximum_pareto_labels_per_node: usize,
    cheap_candidate_limit: usize,
    uncertainty_penalty_sats: u64,
}

impl RouteCacheKey {
    fn new(
        snapshot: &GraphSnapshot,
        request: &RouteSearchRequest,
        config: RouteSearchConfig,
    ) -> Self {
        let mut sources = request.sources.clone();
        sources.sort_unstable();
        sources.dedup();
        let mut destinations = request.destinations.clone();
        destinations.sort_unstable();
        destinations.dedup();
        Self {
            graph_version: snapshot.version,
            amount: request.amount,
            sources,
            destinations,
            maximum_hops: config.maximum_hops,
            maximum_indexed_endpoints: config.maximum_indexed_endpoints,
            maximum_nodes_explored: config.maximum_nodes_explored,
            maximum_edges_explored: config.maximum_edges_explored,
            maximum_pareto_labels_per_node: config.maximum_pareto_labels_per_node,
            cheap_candidate_limit: config.cheap_candidate_limit,
            uncertainty_penalty_sats: config.uncertainty_penalty_sats,
        }
    }
}

#[derive(Clone, Debug)]
struct CachedSearch {
    routes: Arc<[CompactRoute]>,
    metrics: RouteSearchMetrics,
}

#[derive(Debug)]
struct RouteCacheState {
    entries: BTreeMap<RouteCacheKey, Arc<CachedSearch>>,
    order: VecDeque<RouteCacheKey>,
}

/// Bounded cache of cheap-search outputs, explicitly namespaced by graph version.
#[derive(Clone, Debug)]
pub struct RouteCache {
    capacity: NonZeroUsize,
    state: Arc<Mutex<RouteCacheState>>,
}

impl RouteCache {
    /// Creates a bounded cache. A zero capacity is normalized to one entry.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN),
            state: Arc::new(Mutex::new(RouteCacheState {
                entries: BTreeMap::new(),
                order: VecDeque::new(),
            })),
        }
    }

    fn get(&self, key: &RouteCacheKey) -> Option<Arc<CachedSearch>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(key)
            .cloned()
    }

    fn insert(&self, key: RouteCacheKey, result: CachedSearch) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Entry::Occupied(mut entry) = state.entries.entry(key.clone()) {
            entry.insert(Arc::new(result));
            return;
        }
        while state.entries.len() >= self.capacity.get() {
            if let Some(evicted) = state.order.pop_front() {
                state.entries.remove(&evicted);
            } else {
                break;
            }
        }
        state.order.push_back(key.clone());
        state.entries.insert(key, Arc::new(result));
    }

    /// Removes all cache entries. Publication does not require this because
    /// graph version is part of every key; this is solely memory maintenance.
    pub fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.entries.clear();
        state.order.clear();
    }
}

impl Default for RouteCache {
    fn default() -> Self {
        Self::new(4_096)
    }
}

#[derive(Debug)]
struct AdmissionWindow {
    opened_at_seconds: u64,
    requests: usize,
}

#[derive(Debug)]
struct SearchAdmission {
    in_flight: AtomicUsize,
    maximum_in_flight: usize,
    maximum_requests_per_window: usize,
    window_seconds: u64,
    window: Mutex<AdmissionWindow>,
}

impl SearchAdmission {
    fn new(config: RouteSearchConfig) -> Self {
        Self {
            in_flight: AtomicUsize::new(0),
            maximum_in_flight: config.maximum_in_flight_searches,
            maximum_requests_per_window: config.maximum_requests_per_window,
            window_seconds: config.rate_window_seconds,
            window: Mutex::new(AdmissionWindow {
                opened_at_seconds: 0,
                requests: 0,
            }),
        }
    }

    fn try_acquire(&self, now_seconds: u64) -> Result<SearchPermit<'_>, RouteSearchError> {
        let previous = self.in_flight.fetch_add(1, AtomicOrdering::AcqRel);
        if previous >= self.maximum_in_flight {
            self.in_flight.fetch_sub(1, AtomicOrdering::AcqRel);
            return Err(RouteSearchError::Backpressured);
        }
        let mut window = self
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now_seconds.saturating_sub(window.opened_at_seconds) >= self.window_seconds {
            window.opened_at_seconds = now_seconds;
            window.requests = 0;
        }
        if window.requests >= self.maximum_requests_per_window {
            drop(window);
            self.in_flight.fetch_sub(1, AtomicOrdering::AcqRel);
            return Err(RouteSearchError::RateLimited);
        }
        window.requests += 1;
        Ok(SearchPermit { admission: self })
    }
}

struct SearchPermit<'a> {
    admission: &'a SearchAdmission,
}

impl Drop for SearchPermit<'_> {
    fn drop(&mut self) {
        self.admission
            .in_flight
            .fetch_sub(1, AtomicOrdering::AcqRel);
    }
}

/// Aggregate route-search telemetry with fixed-cost counter updates.
#[derive(Clone, Debug)]
pub struct SearchObservability {
    started_at: Instant,
    requests: Arc<AtomicU64>,
    cache_hits: Arc<AtomicU64>,
    candidates_generated: Arc<AtomicU64>,
    nodes_explored: Arc<AtomicU64>,
    edges_explored: Arc<AtomicU64>,
    pareto_pruned: Arc<AtomicU64>,
    feasibility_pruned: Arc<AtomicU64>,
    latency_histogram: Arc<Mutex<LatencyHistogram>>,
}

/// Snapshot of route-search observability counters and latency percentiles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchObservabilityReport {
    /// Completed route-query count.
    pub requests: u64,
    /// Completed queries per elapsed second since telemetry initialization.
    pub qps: u64,
    /// Fraction of completed requests served by the cheap-search cache.
    pub cache_hit_basis_points: u16,
    /// Candidate paths generated by bounded search.
    pub candidates_generated: u64,
    /// Priority-queue labels explored.
    pub nodes_explored: u64,
    /// Adjacency entries explored.
    pub edges_explored: u64,
    /// Pareto-bounded labels pruned.
    pub pareto_pruned: u64,
    /// Definitely impossible edges pruned before detailed scoring.
    pub feasibility_pruned: u64,
    /// Approximate p50 route latency in microseconds.
    pub p50_latency_micros: u64,
    /// Approximate p95 route latency in microseconds.
    pub p95_latency_micros: u64,
    /// Approximate p99 route latency in microseconds.
    pub p99_latency_micros: u64,
}

#[derive(Clone, Debug)]
struct LatencyHistogram {
    /// Bucket `i` represents [2^i, 2^(i + 1)) microseconds; the last bucket
    /// saturates. This avoids a per-query allocation or global route lock.
    buckets: [u64; 64],
    total: u64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; 64],
            total: 0,
        }
    }
}

impl LatencyHistogram {
    fn record(&mut self, latency: Duration) {
        let micros = saturating_u64(latency.as_micros());
        let bucket = micros.max(1).ilog2() as usize;
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.total = self.total.saturating_add(1);
    }

    fn percentile_micros(&self, percentile_basis_points: u16) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let target = (u128::from(self.total) * u128::from(percentile_basis_points))
            .div_ceil(u128::from(BASIS_POINTS));
        let mut cumulative = 0_u64;
        for (index, count) in self.buckets.iter().copied().enumerate() {
            cumulative = cumulative.saturating_add(count);
            if u128::from(cumulative) >= target {
                return 1_u64
                    .checked_shl(u32::try_from(index).unwrap_or(u32::MAX))
                    .unwrap_or(u64::MAX);
            }
        }
        u64::MAX
    }
}

impl Default for SearchObservability {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            requests: Arc::new(AtomicU64::new(0)),
            cache_hits: Arc::new(AtomicU64::new(0)),
            candidates_generated: Arc::new(AtomicU64::new(0)),
            nodes_explored: Arc::new(AtomicU64::new(0)),
            edges_explored: Arc::new(AtomicU64::new(0)),
            pareto_pruned: Arc::new(AtomicU64::new(0)),
            feasibility_pruned: Arc::new(AtomicU64::new(0)),
            latency_histogram: Arc::new(Mutex::new(LatencyHistogram::default())),
        }
    }
}

impl SearchObservability {
    fn record(&self, metrics: RouteSearchMetrics) {
        self.requests.fetch_add(1, AtomicOrdering::Relaxed);
        self.cache_hits
            .fetch_add(u64::from(metrics.cache_hit), AtomicOrdering::Relaxed);
        self.candidates_generated
            .fetch_add(metrics.candidates_generated, AtomicOrdering::Relaxed);
        self.nodes_explored
            .fetch_add(metrics.nodes_explored, AtomicOrdering::Relaxed);
        self.edges_explored
            .fetch_add(metrics.edges_explored, AtomicOrdering::Relaxed);
        self.pareto_pruned
            .fetch_add(metrics.pareto_pruned, AtomicOrdering::Relaxed);
        self.feasibility_pruned
            .fetch_add(metrics.feasibility_pruned, AtomicOrdering::Relaxed);
        self.latency_histogram
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(metrics.latency);
    }

    /// Returns an allocation-free summary appropriate for metrics export.
    #[must_use]
    pub fn report(&self) -> SearchObservabilityReport {
        let requests = self.requests.load(AtomicOrdering::Relaxed);
        let elapsed_seconds = self.started_at.elapsed().as_secs().max(1);
        let cache_hits = self.cache_hits.load(AtomicOrdering::Relaxed);
        let histogram = self
            .latency_histogram
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        SearchObservabilityReport {
            requests,
            qps: requests / elapsed_seconds,
            cache_hit_basis_points: saturating_u16(
                (u128::from(cache_hits) * u128::from(BASIS_POINTS)) / u128::from(requests.max(1)),
            ),
            candidates_generated: self.candidates_generated.load(AtomicOrdering::Relaxed),
            nodes_explored: self.nodes_explored.load(AtomicOrdering::Relaxed),
            edges_explored: self.edges_explored.load(AtomicOrdering::Relaxed),
            pareto_pruned: self.pareto_pruned.load(AtomicOrdering::Relaxed),
            feasibility_pruned: self.feasibility_pruned.load(AtomicOrdering::Relaxed),
            p50_latency_micros: histogram.percentile_micros(5_000),
            p95_latency_micros: histogram.percentile_micros(9_500),
            p99_latency_micros: histogram.percentile_micros(9_900),
        }
    }
}

/// The graph router. Its route hot path holds an immutable snapshot `Arc` and
/// never locks the discovery registry or waits for topology construction.
#[derive(Debug)]
pub struct ScalableRouter {
    publisher: Arc<GraphSnapshotPublisher>,
    cache: RouteCache,
    config: RouteSearchConfig,
    admission: SearchAdmission,
    observability: SearchObservability,
}

impl ScalableRouter {
    /// Creates a router over a snapshot publisher.
    #[must_use]
    pub fn new(publisher: Arc<GraphSnapshotPublisher>, config: RouteSearchConfig) -> Self {
        let config = config.normalized();
        Self {
            publisher,
            cache: RouteCache::default(),
            config,
            admission: SearchAdmission::new(config),
            observability: SearchObservability::default(),
        }
    }

    /// Creates a router with an explicitly sized versioned route cache.
    #[must_use]
    pub fn with_cache(
        publisher: Arc<GraphSnapshotPublisher>,
        config: RouteSearchConfig,
        cache: RouteCache,
    ) -> Self {
        let config = config.normalized();
        Self {
            publisher,
            cache,
            config,
            admission: SearchAdmission::new(config),
            observability: SearchObservability::default(),
        }
    }

    /// Returns the lock-free snapshot publisher for background infrastructure.
    #[must_use]
    pub fn publisher(&self) -> &Arc<GraphSnapshotPublisher> {
        &self.publisher
    }

    /// Returns telemetry for route-search observability.
    #[must_use]
    pub const fn observability(&self) -> &SearchObservability {
        &self.observability
    }

    /// Evaluates a request at a deterministic time, useful for tests and hosts
    /// that already own a monotonic clock for rate-limit policy.
    ///
    /// # Errors
    ///
    /// Returns an admission error when local rate/concurrency bounds are
    /// exceeded, an endpoint error when indexes resolve no usable endpoint, or
    /// a generation mismatch when detail data was not pinned to this graph.
    pub fn evaluate_at(
        &self,
        request: &RouteSearchRequest,
        registry: &ConnectorRegistrySnapshot,
        evaluated_at: EvidenceTimestamp,
    ) -> Result<RouteSearchResult, RouteSearchError> {
        let _permit = self.admission.try_acquire(evaluated_at.unix_seconds())?;
        let started = Instant::now();
        let snapshot = self.publisher.load();
        if snapshot.registry_generation != registry.generation {
            return Err(RouteSearchError::RegistryGenerationMismatch {
                graph: snapshot.registry_generation,
                registry: registry.generation,
            });
        }
        let key = RouteCacheKey::new(&snapshot, request, self.config);
        let (cheap_candidates, mut metrics) = if let Some(cached) = self.cache.get(&key) {
            let mut metrics = cached.metrics;
            metrics.cache_hit = true;
            (cached.routes.to_vec(), metrics)
        } else {
            let (routes, metrics) = cheap_search(&snapshot, request, self.config)?;
            self.cache.insert(
                key,
                CachedSearch {
                    routes: Arc::from(routes.clone()),
                    metrics,
                },
            );
            (routes, metrics)
        };

        // Evidence/risk aggregation and explanation happen only here, after
        // bounded search has cut the graph down to at most `cheap_candidate_limit` paths.
        let ranking = detailed_evaluate(&cheap_candidates, request.amount, registry, evaluated_at);
        let top_k = request.top_k.max(1);
        let mut ranking = ranking;
        ranking.ranked.truncate(top_k);
        metrics.latency = started.elapsed();
        self.observability.record(metrics);
        Ok(RouteSearchResult {
            graph_version: snapshot.version,
            cheap_candidates,
            ranking,
            metrics,
        })
    }
}

fn resolve_endpoints(
    snapshot: &GraphSnapshot,
    endpoints: &[SearchEndpoint],
    maximum_indexed_endpoints: usize,
) -> Vec<CompactConnectorId> {
    let mut resolved = Vec::new();
    for endpoint in endpoints {
        if resolved.len() >= maximum_indexed_endpoints {
            break;
        }
        match *endpoint {
            SearchEndpoint::Connector(id) if snapshot.node(id).is_some() => resolved.push(id),
            SearchEndpoint::Connector(_) => {}
            SearchEndpoint::Capability(capability) => {
                let indexed = snapshot.indexed(capability);
                let take = indexed
                    .len()
                    .min(maximum_indexed_endpoints.saturating_sub(resolved.len()));
                resolved.extend_from_slice(&indexed[..take]);
            }
        }
    }
    resolved.sort_unstable();
    resolved.dedup();
    resolved
}

#[allow(clippy::too_many_lines)]
fn cheap_search(
    snapshot: &GraphSnapshot,
    request: &RouteSearchRequest,
    config: RouteSearchConfig,
) -> Result<(Vec<CompactRoute>, RouteSearchMetrics), RouteSearchError> {
    let sources = resolve_endpoints(snapshot, &request.sources, config.maximum_indexed_endpoints);
    let destinations = resolve_endpoints(
        snapshot,
        &request.destinations,
        config.maximum_indexed_endpoints,
    );
    if sources.is_empty() || destinations.is_empty() {
        return Err(RouteSearchError::EmptyEndpointSet);
    }
    let targets = destinations.into_iter().collect::<BTreeSet<_>>();
    let mut metrics = RouteSearchMetrics::default();
    let mut queue = BinaryHeap::new();
    let mut pareto = vec![Vec::<SearchCost>::new(); snapshot.connector_count()];
    for source in sources {
        let Some(node) = snapshot.node(source) else {
            continue;
        };
        if node.definitely_unavailable_for_search() || !node.supports(Capability::Send) {
            metrics.feasibility_pruned = metrics.feasibility_pruned.saturating_add(1);
            continue;
        }
        let cost = SearchCost {
            uncertainty: node.health().uncertainty_cost(),
            ..SearchCost::zero()
        };
        pareto[source.index()].push(cost);
        queue.push(SearchLabel {
            at: source,
            cost,
            path: vec![source],
            edge_ids: Vec::new(),
        });
    }
    let mut routes = Vec::new();
    while let Some(label) = queue.pop() {
        if metrics.nodes_explored
            >= u64::try_from(config.maximum_nodes_explored).unwrap_or(u64::MAX)
        {
            break;
        }
        metrics.nodes_explored = metrics.nodes_explored.saturating_add(1);
        if label.cost.hops > 0 && targets.contains(&label.at) {
            routes.push(CompactRoute {
                path: label.path.clone(),
                edge_ids: label.edge_ids.clone(),
                estimated_fee: label.cost.fee,
                hop_count: label.cost.hops,
                uncertainty: label.cost.uncertainty,
            });
            metrics.candidates_generated = metrics.candidates_generated.saturating_add(1);
            if routes.len() >= config.cheap_candidate_limit {
                break;
            }
        }
        if label.cost.hops >= config.maximum_hops {
            continue;
        }
        let Some((start, end)) = snapshot.edge_range(label.at) else {
            continue;
        };
        for edge_index in start..end {
            if metrics.edges_explored
                >= u64::try_from(config.maximum_edges_explored).unwrap_or(u64::MAX)
            {
                break;
            }
            metrics.edges_explored = metrics.edges_explored.saturating_add(1);
            let edge_id = edge_id_from_index(edge_index);
            let Some(edge) = snapshot.resolved_edge(edge_id) else {
                continue;
            };
            if edge.disabled || edge.amount_evidence.definitely_insufficient(request.amount) {
                metrics.feasibility_pruned = metrics.feasibility_pruned.saturating_add(1);
                continue;
            }
            let Some(destination) = snapshot.node(edge.base.to) else {
                metrics.feasibility_pruned = metrics.feasibility_pruned.saturating_add(1);
                continue;
            };
            if destination.definitely_unavailable_for_search() {
                metrics.feasibility_pruned = metrics.feasibility_pruned.saturating_add(1);
                continue;
            }
            if label.path.contains(&edge.base.to) {
                metrics.pareto_pruned = metrics.pareto_pruned.saturating_add(1);
                continue;
            }
            let uncertainty = edge
                .amount_evidence
                .state
                .uncertainty_cost()
                .saturating_add(edge.execution_evidence.uncertainty_cost())
                .saturating_add(destination.health().uncertainty_cost());
            let cost = label
                .cost
                .extended(edge.base.estimated_fee(request.amount), uncertainty);
            let labels = &mut pareto[edge.base.to.index()];
            if labels
                .iter()
                .copied()
                // Equal-cost labels can carry distinct connector paths. Keep
                // them (within the existing Pareto bound) so independent
                // sources with identical quotes remain selectable routes.
                .any(|existing| existing.dominates(cost))
            {
                metrics.pareto_pruned = metrics.pareto_pruned.saturating_add(1);
                continue;
            }
            labels.retain(|existing| !cost.dominates(*existing));
            labels.push(cost);
            if labels.len() > config.maximum_pareto_labels_per_node {
                labels.sort_unstable_by(|left, right| {
                    left.priority(config)
                        .cmp(&right.priority(config))
                        .then(left.fee.cmp(&right.fee))
                        .then(left.hops.cmp(&right.hops))
                        .then(left.uncertainty.cmp(&right.uncertainty))
                });
                labels.truncate(config.maximum_pareto_labels_per_node);
                if !labels.contains(&cost) {
                    metrics.pareto_pruned = metrics.pareto_pruned.saturating_add(1);
                    continue;
                }
            }
            let mut path = label.path.clone();
            path.push(edge.base.to);
            let mut edge_ids = label.edge_ids.clone();
            edge_ids.push(edge_id);
            queue.push(SearchLabel {
                at: edge.base.to,
                cost,
                path,
                edge_ids,
            });
        }
    }
    routes.sort_unstable_by(|left, right| {
        (
            left.estimated_fee,
            left.hop_count,
            left.uncertainty,
            &left.path,
            &left.edge_ids,
        )
            .cmp(&(
                right.estimated_fee,
                right.hop_count,
                right.uncertainty,
                &right.path,
                &right.edge_ids,
            ))
    });
    routes.dedup_by(|left, right| left.edge_ids == right.edge_ids);
    Ok((routes, metrics))
}

fn detailed_evaluate(
    routes: &[CompactRoute],
    amount: Amount,
    registry: &ConnectorRegistrySnapshot,
    evaluated_at: EvidenceTimestamp,
) -> RouteRanking {
    let mut evidence = BTreeMap::<ConnectorId, ConnectorEvidence>::new();
    let mut candidates = Vec::new();
    for route in routes {
        let mut hops = Vec::with_capacity(route.path.len().saturating_sub(1));
        let mut complete = true;
        // A graph edge represents execution by its origin connector. The final
        // target receives the transfer and is not fabricated as another hop.
        for compact_id in route
            .path
            .iter()
            .copied()
            .take(route.path.len().saturating_sub(1))
        {
            let Some(connector) = registry.get(compact_id) else {
                complete = false;
                break;
            };
            let snapshot = &connector.snapshot;
            evidence.insert(snapshot.id.clone(), snapshot.evidence.clone());
            hops.push(RouteHop::new(
                snapshot.id.clone(),
                snapshot.connector_type,
                snapshot.capabilities,
                snapshot.liquidity.clone(),
                snapshot.fee.clone(),
                snapshot.reliability.clone(),
            ));
        }
        if complete && let Ok(candidate) = RouteCandidate::new(amount, hops) {
            candidates.push(candidate);
        }
    }
    rank_routes(
        PaymentRequest::new(amount),
        candidates,
        &evidence,
        evaluated_at,
        RouteRankingConfig::default(),
    )
}

/// A bounded discovery ingress queue. Protocol adapters push normalized batches
/// from their asynchronous tasks; it is intentionally outside route queries.
#[derive(Clone, Debug)]
pub struct DiscoveryQueue {
    sender: SyncSender<Vec<DiscoveredConnector>>,
    receiver: Arc<Mutex<Receiver<Vec<DiscoveredConnector>>>>,
}

/// Discovery ingress failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryQueueError {
    /// The bounded queue is full. The adapter should apply its own backoff.
    Backpressured,
    /// The discovery worker was shut down.
    Closed,
}

impl fmt::Display for DiscoveryQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Backpressured => "connector discovery queue is full",
            Self::Closed => "connector discovery worker is unavailable",
        })
    }
}

impl std::error::Error for DiscoveryQueueError {}

impl DiscoveryQueue {
    /// Creates a bounded ingress queue. Zero is normalized to one batch.
    #[must_use]
    pub fn new(maximum_batches: usize) -> Self {
        let (sender, receiver) = sync_channel(maximum_batches.max(1));
        Self {
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    /// Enqueues one normalized discovery batch without blocking the adapter.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryQueueError::Backpressured`] when the bounded queue is
    /// full or [`DiscoveryQueueError::Closed`] when its worker is unavailable.
    pub fn try_submit(&self, updates: Vec<DiscoveredConnector>) -> Result<(), DiscoveryQueueError> {
        self.sender.try_send(updates).map_err(|error| match error {
            TrySendError::Full(_) => DiscoveryQueueError::Backpressured,
            TrySendError::Disconnected(_) => DiscoveryQueueError::Closed,
        })
    }

    fn try_receive(&self) -> Result<Option<Vec<DiscoveredConnector>>, DiscoveryQueueError> {
        self.receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_recv()
            .map(Some)
            .or_else(|error| match error {
                TryRecvError::Empty => Ok(None),
                TryRecvError::Disconnected => Err(DiscoveryQueueError::Closed),
            })
    }
}

/// Compiler boundary between protocol discovery and compact graph topology.
///
/// A compiler may retain adapter-declared edge data and produces a complete,
/// immutable snapshot off-path. Future sharded discovery can implement this
/// trait without changing route-query code.
pub trait GraphSnapshotCompiler: Send + Sync {
    /// Compiles the next immutable graph snapshot from a registry snapshot.
    ///
    /// # Errors
    ///
    /// Returns a graph build error when compiler input cannot form valid compact
    /// topology. Callers retain the last successfully published snapshot.
    fn compile(
        &self,
        registry: &ConnectorRegistrySnapshot,
        version: u64,
    ) -> Result<GraphSnapshot, GraphBuildError>;
}

/// Background-control-plane coordinator for incremental discovery.
///
/// Call [`Self::drain_once`] from a dedicated worker or scheduler. It locks the
/// registry only while applying a small normalized batch, releases it before
/// graph compilation, and atomically publishes only the completed snapshot.
#[derive(Debug)]
pub struct DiscoveryCoordinator {
    queue: DiscoveryQueue,
    registry: Arc<Mutex<ConnectorRegistry>>,
    publisher: Arc<GraphSnapshotPublisher>,
    next_graph_version: AtomicU64,
}

impl DiscoveryCoordinator {
    /// Creates a coordinator with a bounded asynchronous ingress queue.
    #[must_use]
    pub fn new(
        queue_capacity: usize,
        registry: Arc<Mutex<ConnectorRegistry>>,
        publisher: Arc<GraphSnapshotPublisher>,
    ) -> Self {
        let next_graph_version = publisher.load().version().saturating_add(1);
        Self {
            queue: DiscoveryQueue::new(queue_capacity),
            registry,
            publisher,
            next_graph_version: AtomicU64::new(next_graph_version),
        }
    }

    /// Returns a cloneable non-blocking ingress handle for protocol adapters.
    #[must_use]
    pub fn queue(&self) -> DiscoveryQueue {
        self.queue.clone()
    }

    /// Applies at most one queued batch and publishes a rebuilt graph.
    ///
    /// Returns `Ok(false)` when no batch is waiting. Route queries remain on the
    /// latest published snapshot throughout this operation.
    ///
    /// # Errors
    ///
    /// Returns a queue, registry, or graph compiler error. Errors never replace
    /// the currently published graph snapshot.
    pub fn drain_once(
        &self,
        compiler: &dyn GraphSnapshotCompiler,
    ) -> Result<bool, DiscoveryWorkerError> {
        let Some(updates) = self.queue.try_receive()? else {
            return Ok(false);
        };
        let registry_snapshot = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.apply_incremental(updates)?;
            registry.snapshot()
        };
        let version = self.next_graph_version.fetch_add(1, AtomicOrdering::AcqRel);
        let snapshot = compiler.compile(&registry_snapshot, version)?;
        self.publisher.publish(Arc::new(snapshot));
        Ok(true)
    }
}

/// Failure while processing background discovery work.
#[derive(Debug)]
pub enum DiscoveryWorkerError {
    /// Bounded discovery ingress state.
    Queue(DiscoveryQueueError),
    /// Registry normalization failure.
    Registry(RegistryError),
    /// Graph compilation failure.
    Graph(GraphBuildError),
}

impl fmt::Display for DiscoveryWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Queue(error) => write!(formatter, "discovery queue: {error}"),
            Self::Registry(error) => write!(formatter, "connector registry: {error}"),
            Self::Graph(error) => write!(formatter, "graph compilation: {error}"),
        }
    }
}

impl std::error::Error for DiscoveryWorkerError {}

impl From<DiscoveryQueueError> for DiscoveryWorkerError {
    fn from(error: DiscoveryQueueError) -> Self {
        Self::Queue(error)
    }
}

impl From<RegistryError> for DiscoveryWorkerError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<GraphBuildError> for DiscoveryWorkerError {
    fn from(error: GraphBuildError) -> Self {
        Self::Graph(error)
    }
}

/// Standard deterministic large-scale graph sizes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BenchmarkScale {
    /// 100,000 connectors and 200,000 executable edges.
    Connectors100k,
    /// 500,000 connectors and 1,000,000 executable edges.
    Connectors500k,
    /// 1,000,000 connectors and 2,000,000 executable edges.
    Connectors1m,
    /// 5,000,000 connectors and 10,000,000 executable edges.
    Connectors5m,
}

impl BenchmarkScale {
    /// Returns the connector count represented by this benchmark.
    #[must_use]
    pub const fn connector_count(self) -> usize {
        match self {
            Self::Connectors100k => 100_000,
            Self::Connectors500k => 500_000,
            Self::Connectors1m => 1_000_000,
            Self::Connectors5m => 5_000_000,
        }
    }

    /// Returns the exact executable edge count: two sparse edges per node.
    #[must_use]
    pub const fn edge_count(self) -> usize {
        self.connector_count() * 2
    }
}

/// Reproducible graph generator with no protocol/network dependencies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LargeGraphGenerator {
    seed: u64,
}

impl LargeGraphGenerator {
    /// Creates a generator. The same seed and scale produce byte-equivalent
    /// compact graph content and identical route ordering.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Builds a sparse graph with exactly two declared direct edges per node.
    ///
    /// It intentionally generates no external IDs or protocol metadata, which
    /// keeps the benchmark focused on hot-path topology/memory rather than the
    /// control-plane registry's string storage.
    ///
    /// # Errors
    ///
    /// Returns a graph error if the requested scale cannot be represented by
    /// compact node or edge IDs.
    pub fn generate(
        self,
        scale: BenchmarkScale,
        version: u64,
    ) -> Result<GraphSnapshot, GraphBuildError> {
        let count = scale.connector_count();
        let health = HealthObservation::known(
            HealthState::Healthy,
            EvidenceTimestamp::from_unix_seconds(1),
        );
        let capabilities = ConnectorCapabilities::new(true, true, true, true);
        let mut builder = GraphBuilder::new(
            (0..count)
                .map(|index| {
                    let kind = match index % 3 {
                        0 => ConnectorType::Cashu,
                        1 => ConnectorType::Fedimint,
                        _ => ConnectorType::Lightning,
                    };
                    GraphNode::new(kind, capabilities, health)
                })
                .collect(),
        );
        let mut state = self.seed;
        for index in 0..count {
            let from = compact_id_from_index(index);
            let next = compact_id_from_index((index + 1) % count);
            builder.add_edge(generated_edge(from, next, &mut state));
            // A deterministic long edge makes bounded Dijkstra exercise Pareto
            // pruning without manufacturing non-declared paths at query time.
            let stride = (usize::try_from(next_random(&mut state)).unwrap_or(usize::MAX)
                % count.saturating_sub(1).max(1))
                + 1;
            let target = compact_id_from_index((index + stride) % count);
            builder.add_edge(generated_edge(from, target, &mut state));
        }
        builder.build(version)
    }

    /// Runs deterministic bounded cheap searches and reports scale metrics.
    ///
    /// # Errors
    ///
    /// Returns graph construction or bounded-search errors. The latter is kept
    /// explicit rather than assumed impossible so benchmark regressions are
    /// observable.
    pub fn benchmark(
        self,
        scale: BenchmarkScale,
        query_count: usize,
    ) -> Result<BenchmarkReport, BenchmarkError> {
        let snapshot = self.generate(scale, 1)?;
        let config = RouteSearchConfig::default();
        let started = Instant::now();
        let mut aggregate = RouteSearchMetrics::default();
        let mut latencies = Vec::with_capacity(query_count.max(1));
        // Keep the target within the default hop bound while still constructing
        // and retaining the full requested-scale graph.
        let target = compact_id_from_index(1);
        let request = RouteSearchRequest {
            amount: Amount::from_sats(100_000),
            sources: vec![SearchEndpoint::Connector(CompactConnectorId::from_index(0))],
            destinations: vec![SearchEndpoint::Connector(target)],
            top_k: 3,
        };
        for _ in 0..query_count.max(1) {
            let query_started = Instant::now();
            let (_, metrics) = cheap_search(&snapshot, &request, config)?;
            latencies.push(query_started.elapsed());
            aggregate.candidates_generated = aggregate
                .candidates_generated
                .saturating_add(metrics.candidates_generated);
            aggregate.nodes_explored = aggregate
                .nodes_explored
                .saturating_add(metrics.nodes_explored);
            aggregate.edges_explored = aggregate
                .edges_explored
                .saturating_add(metrics.edges_explored);
            aggregate.pareto_pruned = aggregate
                .pareto_pruned
                .saturating_add(metrics.pareto_pruned);
            aggregate.feasibility_pruned = aggregate
                .feasibility_pruned
                .saturating_add(metrics.feasibility_pruned);
        }
        let elapsed = started.elapsed();
        latencies.sort_unstable();
        Ok(BenchmarkReport {
            scale,
            connector_count: snapshot.connector_count(),
            edge_count: snapshot.edge_count(),
            query_count: query_count.max(1),
            qps: saturating_u64(
                (u128::try_from(query_count.max(1)).unwrap_or(u128::MAX) * 1_000_000_000)
                    / elapsed.as_nanos().max(1),
            ),
            p50_latency_micros: benchmark_percentile_micros(&latencies, 5_000),
            p95_latency_micros: benchmark_percentile_micros(&latencies, 9_500),
            p99_latency_micros: benchmark_percentile_micros(&latencies, 9_900),
            estimated_memory_bytes: snapshot.estimated_memory_bytes(),
            // The generator is intentionally single-threaded and CPU-bound;
            // this is portable CPU work time rather than OS-specific process %.
            cpu_work_micros: saturating_u64(elapsed.as_micros()),
            candidates_generated: aggregate.candidates_generated,
            nodes_explored: aggregate.nodes_explored,
            edges_explored: aggregate.edges_explored,
            cache_hit_basis_points: 0,
            pruning_basis_points: ratio_basis_points(
                aggregate
                    .pareto_pruned
                    .saturating_add(aggregate.feasibility_pruned),
                aggregate
                    .edges_explored
                    .saturating_add(aggregate.nodes_explored),
            ),
        })
    }
}

impl Default for LargeGraphGenerator {
    fn default() -> Self {
        Self::new(0xEC_A5_4D_5E_ED)
    }
}

fn generated_edge(
    from: CompactConnectorId,
    to: CompactConnectorId,
    state: &mut u64,
) -> ExecutableEdge {
    let random = next_random(state);
    ExecutableEdge {
        from,
        to,
        mechanism: TransferMechanism::Direct,
        base_fee: Amount::from_sats(random % 20),
        fee_parts_per_million: u32::try_from(random % 100).unwrap_or_default(),
        amount_evidence: AmountAwareEvidence::known(
            Amount::from_sats(10_000_000),
            EvidenceTimestamp::from_unix_seconds(1),
            ConfidenceLevel::Medium,
        ),
        execution_evidence: EvidenceState::Known,
    }
}

const fn next_random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

fn ratio_basis_points(numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 {
        return 0;
    }
    saturating_u16(
        ((u128::from(numerator) * u128::from(BASIS_POINTS)) / u128::from(denominator))
            .min(u128::from(BASIS_POINTS)),
    )
}

fn benchmark_percentile_micros(latencies: &[Duration], percentile_basis_points: u16) -> u64 {
    if latencies.is_empty() {
        return 0;
    }
    let index = ((latencies.len() * usize::from(percentile_basis_points))
        .div_ceil(usize::from(BASIS_POINTS)))
    .saturating_sub(1);
    saturating_u64(latencies[index].as_micros())
}

/// Failure while generating or executing a deterministic router benchmark.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BenchmarkError {
    /// The requested sparse graph could not be compiled.
    Graph(GraphBuildError),
    /// The bounded query configuration could not resolve benchmark endpoints.
    Search(RouteSearchError),
}

impl fmt::Display for BenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Graph(error) => write!(formatter, "graph generation: {error}"),
            Self::Search(error) => write!(formatter, "route search: {error}"),
        }
    }
}

impl std::error::Error for BenchmarkError {}

impl From<GraphBuildError> for BenchmarkError {
    fn from(error: GraphBuildError) -> Self {
        Self::Graph(error)
    }
}

impl From<RouteSearchError> for BenchmarkError {
    fn from(error: RouteSearchError) -> Self {
        Self::Search(error)
    }
}

/// Portable benchmark report. Memory is the exact structural graph allocation
/// estimate; `cpu_work_micros` is intentionally not represented as a host-wide
/// CPU percentage because that value is OS-specific and non-deterministic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BenchmarkReport {
    /// Requested deterministic scale.
    pub scale: BenchmarkScale,
    /// Graph connector count.
    pub connector_count: usize,
    /// Declared executable edge count.
    pub edge_count: usize,
    /// Number of route searches measured.
    pub query_count: usize,
    /// Completed search queries per second.
    pub qps: u64,
    /// p50 sampled search latency.
    pub p50_latency_micros: u64,
    /// p95 search latency.
    pub p95_latency_micros: u64,
    /// p99 search latency.
    pub p99_latency_micros: u64,
    /// Structural graph memory estimate, excluding allocator and registry strings.
    pub estimated_memory_bytes: usize,
    /// CPU-bound work time on the benchmark thread in microseconds.
    pub cpu_work_micros: u64,
    /// Candidate paths created across all searches.
    pub candidates_generated: u64,
    /// Labels explored across all searches.
    pub nodes_explored: u64,
    /// Compact adjacency entries explored across all searches.
    pub edges_explored: u64,
    /// Cache hit rate (zero because generator intentionally exercises raw search).
    pub cache_hit_basis_points: u16,
    /// Fraction of inspected work rejected by feasibility or Pareto pruning.
    pub pruning_basis_points: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConnectorHealth, Evidence, EvidenceSource, FeeQuote, LiquidityInfo, ReliabilityInfo,
        SolvencyStatus,
    };

    const NOW: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(10_000);

    fn capabilities() -> ConnectorCapabilities {
        ConnectorCapabilities::new(true, true, true, true)
    }

    fn connector(id: &str) -> DiscoveredConnector {
        let id = ConnectorId::new(id).expect("valid connector id");
        let liquidity = Evidence::reported(
            LiquidityInfo::new(Amount::from_sats(1_000_000), None),
            EvidenceSource::Observer,
            NOW,
            ConfidenceLevel::High,
        );
        let fee = Evidence::reported(
            FeeQuote::new(Amount::from_sats(10)),
            EvidenceSource::Observer,
            NOW,
            ConfidenceLevel::High,
        );
        let reliability = Evidence::reported(
            ReliabilityInfo::new(9_900, 500).expect("valid reliability"),
            EvidenceSource::Historical,
            NOW,
            ConfidenceLevel::High,
        );
        let evidence = ConnectorEvidence::new(
            id.clone(),
            Some(EvidenceTimestamp::from_unix_seconds(1)),
            Evidence::reported(
                ConnectorHealth::Healthy,
                EvidenceSource::Observer,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                SolvencyStatus::Supported,
                EvidenceSource::Independent,
                NOW,
                ConfidenceLevel::High,
            ),
            Evidence::reported(
                ReliabilityInfo::new(9_900, 500).expect("valid reliability"),
                EvidenceSource::Historical,
                NOW,
                ConfidenceLevel::High,
            ),
        );
        DiscoveredConnector {
            snapshot: ConnectorSnapshot {
                id,
                connector_type: ConnectorType::Cashu,
                capabilities: capabilities(),
                liquidity,
                fee,
                reliability,
                evidence,
            },
            health: HealthObservation::known(HealthState::Healthy, NOW),
        }
    }

    fn known_edge(from: u32, to: u32, fee: u64) -> ExecutableEdge {
        ExecutableEdge {
            from: CompactConnectorId::from_index(from),
            to: CompactConnectorId::from_index(to),
            mechanism: TransferMechanism::Direct,
            base_fee: Amount::from_sats(fee),
            fee_parts_per_million: 0,
            amount_evidence: AmountAwareEvidence::known(
                Amount::from_sats(1_000_000),
                NOW,
                ConfidenceLevel::High,
            ),
            execution_evidence: EvidenceState::Known,
        }
    }

    fn registry_and_snapshot() -> (ConnectorRegistrySnapshot, GraphSnapshot) {
        let mut registry = ConnectorRegistry::default();
        registry
            .apply_incremental([
                connector("cashu:a"),
                connector("cashu:b"),
                connector("cashu:c"),
            ])
            .expect("registry update");
        let details = registry.snapshot();
        let nodes = details
            .connectors
            .iter()
            .map(|record| GraphNode::new(record.connector_type, record.capabilities, record.health))
            .collect();
        let mut builder = GraphBuilder::new(nodes).registry_generation(details.generation());
        builder.extend_edges([
            known_edge(0, 1, 20),
            known_edge(1, 2, 5),
            known_edge(0, 2, 40),
        ]);
        let snapshot = builder.build(1).expect("graph");
        (details, snapshot)
    }

    fn request() -> RouteSearchRequest {
        RouteSearchRequest {
            amount: Amount::from_sats(100_000),
            sources: vec![SearchEndpoint::Connector(CompactConnectorId::from_index(0))],
            destinations: vec![SearchEndpoint::Connector(CompactConnectorId::from_index(2))],
            top_k: 3,
        }
    }

    #[test]
    fn registry_assigns_compact_ids_deterministically_and_updates_in_place() {
        let mut registry = ConnectorRegistry::default();
        let report = registry
            .apply_incremental([connector("cashu:z"), connector("cashu:a")])
            .expect("registry update");
        assert_eq!(report.inserted, 2);
        assert_eq!(
            registry.resolve(&ConnectorId::new("cashu:a").expect("id")),
            Some(CompactConnectorId::from_index(0))
        );
        assert_eq!(
            registry.resolve(&ConnectorId::new("cashu:z").expect("id")),
            Some(CompactConnectorId::from_index(1))
        );
        let report = registry
            .apply_incremental([connector("cashu:a")])
            .expect("update");
        assert_eq!(report.updated, 1);
        assert_eq!(registry.snapshot().len(), 2);
    }

    #[test]
    fn registry_rejects_ambiguous_same_batch_updates() {
        let mut registry = ConnectorRegistry::default();
        assert!(matches!(
            registry
                .apply_incremental([connector("cashu:duplicate"), connector("cashu:duplicate")]),
            Err(RegistryError::DuplicateUpdate { .. })
        ));
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn capability_endpoint_generation_is_explicitly_bounded() {
        let (_, snapshot) = registry_and_snapshot();
        assert_eq!(
            resolve_endpoints(
                &snapshot,
                &[SearchEndpoint::Capability(Capability::Send)],
                1
            ),
            vec![CompactConnectorId::from_index(0)]
        );
        assert_eq!(
            resolve_endpoints(
                &snapshot,
                &[SearchEndpoint::Capability(Capability::Send)],
                2
            ),
            vec![
                CompactConnectorId::from_index(0),
                CompactConnectorId::from_index(1)
            ]
        );
    }

    #[test]
    fn route_search_uses_only_explicit_edges_not_shared_protocol_type() {
        let (details, mut snapshot) = registry_and_snapshot();
        // Remove every edge to c. Nodes remain Cashu-capable, but that must not
        // create an implicit mint-to-mint route.
        let mut builder = GraphBuilder::new(
            details
                .connectors
                .iter()
                .map(|record| {
                    GraphNode::new(record.connector_type, record.capabilities, record.health)
                })
                .collect(),
        )
        .registry_generation(snapshot.registry_generation());
        builder.add_edge(known_edge(0, 1, 1));
        snapshot = builder.build(2).expect("graph");
        let outcome =
            cheap_search(&snapshot, &request(), RouteSearchConfig::default()).expect("search");
        assert!(outcome.0.is_empty());
    }

    #[test]
    fn known_insufficient_capacity_is_rejected_but_unknown_and_conflicting_stay_explicit() {
        let (details, _) = registry_and_snapshot();
        let mut builder = GraphBuilder::new(
            details
                .connectors
                .iter()
                .map(|record| {
                    GraphNode::new(record.connector_type, record.capabilities, record.health)
                })
                .collect(),
        );
        let mut insufficient = known_edge(0, 1, 1);
        insufficient.amount_evidence =
            AmountAwareEvidence::known(Amount::from_sats(99_999), NOW, ConfidenceLevel::High);
        let mut unknown = known_edge(0, 2, 2);
        unknown.amount_evidence = AmountAwareEvidence::unknown();
        let mut conflicting = known_edge(0, 2, 1);
        conflicting.amount_evidence = AmountAwareEvidence::conflicting(Some(NOW));
        builder.extend_edges([insufficient, unknown, conflicting]);
        let graph = builder.build(1).expect("graph");
        let (routes, metrics) =
            cheap_search(&graph, &request(), RouteSearchConfig::default()).expect("search");
        assert_eq!(routes.len(), 2);
        assert_eq!(
            routes[0].uncertainty,
            EvidenceState::Conflicting.uncertainty_cost()
        );
        assert_eq!(
            routes[1].uncertainty,
            EvidenceState::Unknown.uncertainty_cost()
        );
        assert_eq!(metrics.feasibility_pruned, 1);
    }

    #[test]
    fn bounded_search_returns_deterministic_top_k_after_detailed_evaluation() {
        let (details, snapshot) = registry_and_snapshot();
        let publisher = Arc::new(GraphSnapshotPublisher::new(Arc::new(snapshot)));
        let router =
            ScalableRouter::with_cache(publisher, RouteSearchConfig::default(), RouteCache::new(8));
        let first = router
            .evaluate_at(&request(), &details, NOW)
            .expect("route evaluation");
        let second = router
            .evaluate_at(&request(), &details, NOW)
            .expect("route evaluation");
        assert_eq!(first.cheap_candidates, second.cheap_candidates);
        assert_eq!(first.ranking.ranked, second.ranking.ranked);
        assert_eq!(
            first.cheap_candidates[0].path,
            vec![
                CompactConnectorId::from_index(0),
                CompactConnectorId::from_index(1),
                CompactConnectorId::from_index(2)
            ]
        );
        assert!(second.metrics.cache_hit);
        assert!(second.ranking.ranked.len() <= 3);
    }

    #[test]
    fn equal_cost_paths_from_distinct_sources_remain_alternatives() {
        let (details, _) = registry_and_snapshot();
        let nodes = details
            .connectors
            .iter()
            .map(|record| GraphNode::new(record.connector_type, record.capabilities, record.health))
            .collect();
        let mut builder = GraphBuilder::new(nodes).registry_generation(details.generation());
        builder.extend_edges([known_edge(0, 2, 20), known_edge(1, 2, 20)]);
        let snapshot = builder.build(2).expect("graph");
        let request = RouteSearchRequest {
            amount: Amount::from_sats(100_000),
            sources: vec![
                SearchEndpoint::Connector(CompactConnectorId::from_index(0)),
                SearchEndpoint::Connector(CompactConnectorId::from_index(1)),
            ],
            destinations: vec![SearchEndpoint::Connector(CompactConnectorId::from_index(2))],
            top_k: 3,
        };

        let (routes, _) =
            cheap_search(&snapshot, &request, RouteSearchConfig::default()).expect("search");
        assert_eq!(routes.len(), 2);
        assert_eq!(
            routes[0].path,
            vec![
                CompactConnectorId::from_index(0),
                CompactConnectorId::from_index(2)
            ]
        );
        assert_eq!(
            routes[1].path,
            vec![
                CompactConnectorId::from_index(1),
                CompactConnectorId::from_index(2)
            ]
        );
    }

    #[test]
    fn graph_version_invalidates_the_route_cache_without_a_route_path_lock() {
        let (details, snapshot) = registry_and_snapshot();
        let publisher = Arc::new(GraphSnapshotPublisher::new(Arc::new(snapshot.clone())));
        let router = ScalableRouter::with_cache(
            Arc::clone(&publisher),
            RouteSearchConfig::default(),
            RouteCache::new(8),
        );
        let first = router
            .evaluate_at(&request(), &details, NOW)
            .expect("first");
        let cached = router
            .evaluate_at(&request(), &details, NOW)
            .expect("cached");
        assert!(!first.metrics.cache_hit);
        assert!(cached.metrics.cache_hit);
        let overlay = GraphOverlay::new(
            2,
            [EdgeOverlay {
                edge_id: EdgeId::from_index(1),
                disabled: true,
                amount_evidence: None,
                execution_evidence: None,
            }],
        );
        publisher.publish(Arc::new(snapshot.with_overlay(2, overlay)));
        let refreshed = router
            .evaluate_at(&request(), &details, NOW)
            .expect("refreshed");
        assert!(!refreshed.metrics.cache_hit);
        assert_eq!(refreshed.cheap_candidates[0].hop_count, 2);
    }

    #[test]
    fn rate_limit_is_deterministic_and_does_not_depend_on_search_results() {
        let (details, snapshot) = registry_and_snapshot();
        let publisher = Arc::new(GraphSnapshotPublisher::new(Arc::new(snapshot)));
        let config = RouteSearchConfig {
            maximum_requests_per_window: 1,
            ..RouteSearchConfig::default()
        };
        let router = ScalableRouter::new(publisher, config);
        router
            .evaluate_at(&request(), &details, NOW)
            .expect("first request");
        assert_eq!(
            router.evaluate_at(&request(), &details, NOW),
            Err(RouteSearchError::RateLimited)
        );
        router
            .evaluate_at(
                &request(),
                &details,
                EvidenceTimestamp::from_unix_seconds(NOW.unix_seconds() + 1),
            )
            .expect("next window");
    }

    #[test]
    fn discovery_queue_applies_incrementally_outside_the_router() {
        struct Compiler;
        impl GraphSnapshotCompiler for Compiler {
            fn compile(
                &self,
                registry: &ConnectorRegistrySnapshot,
                version: u64,
            ) -> Result<GraphSnapshot, GraphBuildError> {
                GraphBuilder::new(
                    registry
                        .connectors
                        .iter()
                        .map(|record| {
                            GraphNode::new(
                                record.connector_type,
                                record.capabilities,
                                record.health,
                            )
                        })
                        .collect(),
                )
                .registry_generation(registry.generation())
                .build(version)
            }
        }
        let initial = GraphBuilder::new(Vec::new())
            .build(1)
            .expect("initial graph");
        let publisher = Arc::new(GraphSnapshotPublisher::new(Arc::new(initial)));
        let coordinator = DiscoveryCoordinator::new(
            1,
            Arc::new(Mutex::new(ConnectorRegistry::default())),
            Arc::clone(&publisher),
        );
        coordinator
            .queue()
            .try_submit(vec![connector("cashu:worker")])
            .expect("queue");
        assert!(coordinator.drain_once(&Compiler).expect("worker pass"));
        assert_eq!(publisher.load().connector_count(), 1);
        assert!(!coordinator.drain_once(&Compiler).expect("empty queue"));
    }

    #[test]
    fn benchmark_scales_are_fixed_and_generator_is_reproducible() {
        assert_eq!(BenchmarkScale::Connectors100k.connector_count(), 100_000);
        assert_eq!(BenchmarkScale::Connectors500k.edge_count(), 1_000_000);
        assert_eq!(BenchmarkScale::Connectors1m.edge_count(), 2_000_000);
        assert_eq!(BenchmarkScale::Connectors5m.edge_count(), 10_000_000);
        let generator = LargeGraphGenerator::new(7);
        let first = generator
            .generate(BenchmarkScale::Connectors100k, 1)
            .expect("graph");
        let second = generator
            .generate(BenchmarkScale::Connectors100k, 1)
            .expect("graph");
        assert_eq!(first.connector_count(), second.connector_count());
        assert_eq!(first.edge_count(), second.edge_count());
        assert_eq!(
            first.estimated_memory_bytes(),
            second.estimated_memory_bytes()
        );
    }
}
