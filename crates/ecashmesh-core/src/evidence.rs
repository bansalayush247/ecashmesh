//! Connector evidence, provenance, freshness, and aggregation primitives.

use crate::model::{ConnectorId, ReliabilityInfo};

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

/// The deterministic result of combining multiple observations for one fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceAggregate<T> {
    /// Agreeing current observations, represented by the strongest/latest one.
    Known(EvidenceObservation<T>),
    /// No usable observations were supplied.
    Unknown,
    /// Agreeing but stale observations, represented by the strongest/latest one.
    Stale(EvidenceObservation<T>),
    /// Usable observations disagree about the underlying value.
    Conflicting {
        /// All conflicting observations, retained for inspection.
        observations: Vec<EvidenceObservation<T>>,
    },
}

impl<T: Clone + Eq> Evidence<T> {
    /// Aggregates observations deterministically.
    ///
    /// Unknown inputs do not add a value. Any disagreement among usable values
    /// is preserved as [`EvidenceAggregate::Conflicting`]. Otherwise, the
    /// newest observation wins; confidence and source provide deterministic
    /// tie-breakers. A current observation wins over a stale one.
    #[must_use]
    pub fn aggregate(inputs: impl IntoIterator<Item = Self>) -> EvidenceAggregate<T> {
        let mut current = Vec::new();
        let mut stale = Vec::new();

        for evidence in inputs {
            match evidence {
                Self::Known(observation) => current.push(observation),
                Self::Stale(observation) => stale.push(observation),
                Self::Unknown => {}
            }
        }

        let all_observations = current.iter().chain(&stale).collect::<Vec<_>>();
        let Some(first) = all_observations.first() else {
            return EvidenceAggregate::Unknown;
        };
        if all_observations
            .iter()
            .any(|observation| observation.value != first.value)
        {
            return EvidenceAggregate::Conflicting {
                observations: current.into_iter().chain(stale).collect(),
            };
        }

        if let Some(best) = select_best(current) {
            EvidenceAggregate::Known(best)
        } else if let Some(best) = select_best(stale) {
            EvidenceAggregate::Stale(best)
        } else {
            EvidenceAggregate::Unknown
        }
    }
}

fn select_best<T>(mut observations: Vec<EvidenceObservation<T>>) -> Option<EvidenceObservation<T>> {
    observations.sort_unstable_by(|left, right| {
        left.observed_at
            .cmp(&right.observed_at)
            .then(left.confidence.cmp(&right.confidence))
            .then(left.source.cmp(&right.source))
    });
    observations.pop()
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(100);
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

    #[test]
    fn aggregate_prefers_the_latest_agreeing_fresh_observation() {
        let aggregate = Evidence::aggregate([
            Evidence::reported(7_u8, EvidenceSource::Connector, OLD, ConfidenceLevel::High),
            Evidence::reported(7_u8, EvidenceSource::Observer, NEW, ConfidenceLevel::Low),
        ]);

        assert_eq!(
            aggregate,
            EvidenceAggregate::Known(EvidenceObservation::new(
                7,
                EvidenceSource::Observer,
                NEW,
                ConfidenceLevel::Low,
            ))
        );
    }

    #[test]
    fn aggregate_retains_stale_and_missing_states() {
        let stale = Evidence::aggregate([Evidence::reported_stale(
            7_u8,
            EvidenceSource::Historical,
            OLD,
            ConfidenceLevel::Medium,
        )]);
        let missing = Evidence::<u8>::aggregate([Evidence::unknown(), Evidence::unknown()]);

        assert!(matches!(stale, EvidenceAggregate::Stale(_)));
        assert_eq!(missing, EvidenceAggregate::Unknown);
    }

    #[test]
    fn aggregate_preserves_conflicting_observations_for_inspection() {
        let aggregate = Evidence::aggregate([
            Evidence::reported(7_u8, EvidenceSource::Connector, OLD, ConfidenceLevel::High),
            Evidence::reported(
                8_u8,
                EvidenceSource::Independent,
                NEW,
                ConfidenceLevel::High,
            ),
        ]);

        assert_eq!(
            aggregate,
            EvidenceAggregate::Conflicting {
                observations: vec![
                    EvidenceObservation::new(
                        7,
                        EvidenceSource::Connector,
                        OLD,
                        ConfidenceLevel::High
                    ),
                    EvidenceObservation::new(
                        8,
                        EvidenceSource::Independent,
                        NEW,
                        ConfidenceLevel::High
                    ),
                ],
            }
        );
    }
}
