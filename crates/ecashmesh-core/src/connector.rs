//! Protocol-independent adapter boundary. Observations are not execution authority.

use crate::{
    Amount, ConnectorCapabilities, ConnectorEvidence, ConnectorId, ConnectorType, Evidence,
    FeeQuote, LiquidityInfo, ReliabilityInfo, RouteCandidate, RouteCandidateError, RouteHop,
};

/// Normalized connector facts supplied by a protocol adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectorSnapshot {
    /// Stable connector identifier.
    pub id: ConnectorId,
    /// Protocol family.
    pub connector_type: ConnectorType,
    /// Abilities available for the evaluated payment.
    pub capabilities: ConnectorCapabilities,
    /// Observed spendable liquidity, not advertised transaction limits.
    pub liquidity: Evidence<LiquidityInfo>,
    /// Payment fee estimate, if actually available.
    pub fee: Evidence<FeeQuote>,
    /// Payment outcome reliability, not HTTP endpoint uptime.
    pub reliability: Evidence<ReliabilityInfo>,
    /// Connector observations and provenance.
    pub evidence: ConnectorEvidence,
}

impl ConnectorSnapshot {
    /// Constructs a one-hop candidate without calculating any score.
    ///
    /// # Errors
    /// Returns a candidate construction error if the one-hop invariant is violated.
    pub fn quote(&self, amount: Amount) -> Result<RouteCandidate, RouteCandidateError> {
        RouteCandidate::new(
            amount,
            vec![RouteHop::new(
                self.id.clone(),
                self.connector_type,
                self.capabilities,
                self.liquidity.clone(),
                self.fee.clone(),
                self.reliability.clone(),
            )],
        )
    }
}
