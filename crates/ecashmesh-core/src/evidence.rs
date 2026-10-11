//! Connector evidence, provenance, freshness, and aggregation primitives.

use crate::model::{ConnectorId, ReliabilityInfo};
use crate::risk::EvidenceField;

/// A Unix timestamp in whole seconds associated with an observation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EvidenceTimestamp(u64);

impl EvidenceTimestamp {
    /// Creates a timestamp from seconds since the Unix epoch.
    #[must_use]
    pub const fn from_unix_seconds(seconds: u64) -> Self {
        Self(seconds)
    }

    /// Returns seconds since the Unix epoch.
    #[must_use]
    pub const fn unix_seconds(self) -> u64 {
        self.0
    }
}

/// The origin of a connector observation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EvidenceSource {
    /// Data reported by the connector or its adapter.
    Connector,
    /// A measurement made by an `EcashMesh` observer.
    Observer,
    /// An independent monitor, auditor, or reputation service.
    Independent,
    /// Historical outcomes recorded by the caller.
    Historical,
}

/// How current an evidence value is.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EvidenceFreshness {
    /// A current observation is available.
    Fresh,
    /// A previous observation is retained but no longer current.
    Stale,
    /// No usable observation is available.
    Unknown,
}

/// The reported confidence in an observation.
///
/// Confidence describes evidence strength, not whether a connector is safe or
/// suitable for a route.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ConfidenceLevel {
    /// The source supplied no confidence assessment.
    None,
    /// The evidence is weak or has a limited basis.
    Low,
    /// The evidence has a reasonable but incomplete basis.
    Medium,
    /// The evidence has a strong basis.
    High,
}

impl ConfidenceLevel {
    /// Returns whether the level should be surfaced as weak evidence.
    #[must_use]
    pub const fn is_weak(self) -> bool {
        matches!(self, Self::None | Self::Low)
    }
}

/// A value together with the facts needed to inspect its provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceObservation<T> {
    /// The observed value.
    pub value: T,
    /// Who supplied or measured the observation.
    pub source: EvidenceSource,
    /// When the value was observed.
    pub observed_at: EvidenceTimestamp,
    /// The source's confidence in this observation.
    pub confidence: ConfidenceLevel,
}

impl<T> EvidenceObservation<T> {
    /// Creates a provenance-carrying observation.
    #[must_use]
    pub const fn new(
        value: T,
        source: EvidenceSource,
        observed_at: EvidenceTimestamp,
        confidence: ConfidenceLevel,
    ) -> Self {
        Self {
            value,
            source,
            observed_at,
            confidence,
        }
    }
}

/// An observed fact, with explicit availability and freshness states.
///
/// `Unknown` is the absence of evidence. It is deliberately distinct from an
/// observed negative value, such as [`ConnectorHealth::Unavailable`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Evidence<T> {
    /// A current observation.
    Known(EvidenceObservation<T>),
    /// No usable observation is available.
    Unknown,
    /// A retained observation that must not be treated as current.
    Stale(EvidenceObservation<T>),
}

impl<T> Evidence<T> {
    /// Creates current evidence from a connector-reported, medium-confidence value.
    ///
    /// Prefer [`Self::reported`] when the source or confidence differs.
    #[must_use]
    pub const fn known(value: T, observed_at: EvidenceTimestamp) -> Self {
        Self::reported(
            value,
            EvidenceSource::Connector,
            observed_at,
            ConfidenceLevel::Medium,
        )
    }

    /// Creates current evidence with explicit provenance and confidence.
    #[must_use]
    pub const fn reported(
        value: T,
        source: EvidenceSource,
        observed_at: EvidenceTimestamp,
        confidence: ConfidenceLevel,
    ) -> Self {
        Self::Known(EvidenceObservation::new(
            value,
            source,
            observed_at,
            confidence,
        ))
    }

    /// Creates unknown evidence.
    #[must_use]
    pub const fn unknown() -> Self {
        Self::Unknown
    }

    /// Creates stale, connector-reported, medium-confidence evidence.
    ///
    /// Prefer [`Self::reported_stale`] when the source or confidence differs.
    #[must_use]
    pub const fn stale(value: T, observed_at: EvidenceTimestamp) -> Self {
        Self::reported_stale(
            value,
            EvidenceSource::Connector,
            observed_at,
            ConfidenceLevel::Medium,
        )
    }

    /// Creates stale evidence with explicit provenance and confidence.
    #[must_use]
    pub const fn reported_stale(
        value: T,
        source: EvidenceSource,
        observed_at: EvidenceTimestamp,
        confidence: ConfidenceLevel,
    ) -> Self {
        Self::Stale(EvidenceObservation::new(
            value,
            source,
            observed_at,
            confidence,
        ))
    }

    /// Returns the observation value for known or stale evidence.
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Known(observation) | Self::Stale(observation) => Some(&observation.value),
            Self::Unknown => None,
        }
    }

    /// Returns the full observation for known or stale evidence.
    #[must_use]
    pub const fn observation(&self) -> Option<&EvidenceObservation<T>> {
        match self {
            Self::Known(observation) | Self::Stale(observation) => Some(observation),
            Self::Unknown => None,
        }
    }

    /// Returns the observation timestamp for known or stale evidence.
    #[must_use]
    pub const fn observed_at(&self) -> Option<EvidenceTimestamp> {
        match self {
            Self::Known(observation) | Self::Stale(observation) => Some(observation.observed_at),
            Self::Unknown => None,
        }
    }

    /// Returns the explicit freshness state.
    #[must_use]
    pub const fn freshness(&self) -> EvidenceFreshness {
        match self {
            Self::Known(_) => EvidenceFreshness::Fresh,
            Self::Unknown => EvidenceFreshness::Unknown,
            Self::Stale(_) => EvidenceFreshness::Stale,
        }
    }

    /// Returns whether the evidence is a current observation.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// Returns whether the evidence is unavailable.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Returns whether the evidence is retained but stale.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        matches!(self, Self::Stale(_))
    }

    /// Maps an observed value while preserving state, provenance, and confidence.
    #[must_use]
    pub fn map<U>(self, transform: impl FnOnce(T) -> U) -> Evidence<U> {
        match self {
            Self::Known(observation) => Evidence::reported(
                transform(observation.value),
                observation.source,
                observation.observed_at,
                observation.confidence,
            ),
            Self::Unknown => Evidence::Unknown,
            Self::Stale(observation) => Evidence::reported_stale(
                transform(observation.value),
                observation.source,
                observation.observed_at,
                observation.confidence,
            ),
        }
    }
}

/// The observed operational state of a connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectorHealth {
    /// The connector is operating normally according to the source.
    Healthy,
    /// The connector is operating with a known impairment.
    Degraded,
    /// The connector is known to be unavailable.
    Unavailable,
}

/// A connector's observed solvency state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolvencyStatus {
    /// Evidence indicates the connector's obligations appear covered.
    Supported,
    /// Evidence indicates an unresolved solvency concern.
    Concerning,
}

/// Evidence known about one connector at a specific evaluation time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectorEvidence {
    /// Connector described by this evidence set.
    pub connector_id: ConnectorId,
    /// When the connector was first observed, if ever.
    pub first_observed_at: Option<EvidenceTimestamp>,
    /// Latest health observation.
    pub health: Evidence<ConnectorHealth>,
    /// Latest solvency observation.
    pub solvency: Evidence<SolvencyStatus>,
    /// Recent payment reliability observation.
    pub reliability: Evidence<ReliabilityInfo>,
    /// Facts for which usable independent observations disagreed. The
    /// corresponding field must then be [`Evidence::Unknown`]: no single
    /// disputed value is trusted, and the disagreement itself is the risk.
    pub conflicting: Vec<EvidenceField>,
}

impl ConnectorEvidence {
    /// Creates evidence for one connector.
    #[must_use]
    pub fn new(
        connector_id: ConnectorId,
        first_observed_at: Option<EvidenceTimestamp>,
        health: Evidence<ConnectorHealth>,
        solvency: Evidence<SolvencyStatus>,
        reliability: Evidence<ReliabilityInfo>,
    ) -> Self {
        Self {
            connector_id,
            first_observed_at,
            health,
            solvency,
            reliability,
            conflicting: Vec::new(),
        }
    }

    /// Records that independent observations of `field` disagreed.
    #[must_use]
    pub fn with_conflicting(mut self, field: EvidenceField) -> Self {
        if !self.conflicting.contains(&field) {
            self.conflicting.push(field);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(200);

    #[test]
    fn evidence_keeps_unknown_distinct_from_an_observed_negative_value() {
        let unknown: Evidence<ConnectorHealth> = Evidence::unknown();
        let unavailable = Evidence::reported(
            ConnectorHealth::Unavailable,
            EvidenceSource::Observer,
            NEW,
            ConfidenceLevel::High,
        );

        assert_eq!(unknown.freshness(), EvidenceFreshness::Unknown);
        assert_eq!(unknown.value(), None);
        assert_eq!(unavailable.freshness(), EvidenceFreshness::Fresh);
        assert_eq!(unavailable.value(), Some(&ConnectorHealth::Unavailable));
    }
}
