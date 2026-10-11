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
