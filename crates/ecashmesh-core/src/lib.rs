//! Protocol-agnostic domain types for describing `EcashMesh` routing inputs and
//! outputs.
//!
//! Protocol adapters translate external data into these types before it reaches
//! the deterministic routing layer. No protocol clients or networking live here.

#![forbid(unsafe_code)]

pub mod connector;
pub mod evidence;
pub mod explain;
pub mod intent;
mod model;
pub mod risk;
pub mod routing;
pub mod scalable;
pub mod simulator;

pub use connector::ConnectorSnapshot;
pub use evidence::{
    ConfidenceLevel, ConnectorEvidence, ConnectorHealth, Evidence, EvidenceAggregate,
    EvidenceFreshness, EvidenceObservation, EvidenceSource, EvidenceTimestamp, SolvencyStatus,
};
pub use explain::{
    AlternativeExplanation, DecisionReason, DecisionReasonCode, ExplainedRoute,
    RejectedRouteExplanation, RouteDecisionExplanation, explain_ranking,
};
pub use intent::{LightningInvoice, LightningInvoiceError};
pub use model::{
    Amount, AmountError, ConnectorCapabilities, ConnectorId, ConnectorIdError, ConnectorType,
    FeeQuote, LiquidityInfo, ReliabilityInfo, Route, RouteCandidate, RouteCandidateError,
    RouteExplanation, RouteHop, RouteQuality,
};
pub use risk::{EvidenceField, EvidenceRiskPolicy, RiskFactor, evaluate_connector_evidence};
pub use routing::{
    PaymentRequest, RankedRoute, RejectedRoute, RiskPenaltyPolicy, RouteRanking,
    RouteRankingConfig, RouteRejectionReason, RouteSignalWeights, RouteSignals, rank_routes,
};
pub use scalable::{
    AmountAwareEvidence, BenchmarkError, BenchmarkReport, BenchmarkScale, Capability,
    CompactConnectorId, ConnectorRegistry, ConnectorRegistrySnapshot, DiscoveredConnector,
    DiscoveryCoordinator, DiscoveryQueue, DiscoveryQueueError, DiscoveryWorkerError, EdgeId,
    EdgeOverlay, EvidenceState, ExecutableEdge, GraphBuildError, GraphBuilder, GraphNode,
    GraphOverlay, GraphSnapshot, GraphSnapshotCompiler, GraphSnapshotPublisher, HealthObservation,
    HealthState, LargeGraphGenerator, RegisteredConnector, RegistryError, RegistryUpdateReport,
    RouteCache, RouteSearchConfig, RouteSearchError, RouteSearchMetrics, RouteSearchRequest,
    RouteSearchResult, ScalableRouter, SearchEndpoint, SearchObservability,
    SearchObservabilityReport, TransferMechanism,
};
pub use simulator::{
    DEMO_EVALUATED_AT, RoutingScenario, ScenarioError, ScenarioName, ScenarioResult,
    SimulatedConnector, all_scenarios, demo_connectors, run_all_scenarios, verify_all_scenarios,
};
