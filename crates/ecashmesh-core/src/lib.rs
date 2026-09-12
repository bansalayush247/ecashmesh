//! Protocol-agnostic domain types for describing `EcashMesh` routing inputs and
//! outputs.
//!
//! This crate deliberately contains no connector implementations, networking,
//! scoring, or pathfinding. Protocol adapters translate external data into
//! these types before it reaches the routing layer.

#![forbid(unsafe_code)]

mod model;

pub use model::{
    Amount, AmountError, ConnectorCapabilities, ConnectorId, ConnectorIdError, ConnectorType,
    Evidence, EvidenceTimestamp, FeeQuote, LiquidityInfo, ReliabilityInfo, RiskFactor, Route,
    RouteCandidate, RouteCandidateError, RouteExplanation, RouteHop, RouteQuality,
};
