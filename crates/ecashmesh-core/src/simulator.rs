//! Deterministic connector fixtures for exercising route ranking without live services.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::evidence::{
    ConfidenceLevel, ConnectorEvidence, ConnectorHealth, Evidence, EvidenceSource,
    EvidenceTimestamp, SolvencyStatus,
};
use crate::model::{
    Amount, ConnectorCapabilities, ConnectorId, ConnectorIdError, ConnectorType, FeeQuote,
    LiquidityInfo, ReliabilityInfo, RouteCandidate, RouteCandidateError, RouteHop,
};
use crate::routing::{PaymentRequest, RouteRanking, RouteRankingConfig, rank_routes};

/// Stable identifier for a built-in simulator fixture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScenarioName {
    /// A healthy, well-funded connector can provide a route.
    HealthyHighLiquidity,
    /// A connector with insufficient current liquidity is rejected before scoring.
    LowLiquidityExcluded,
    /// A lower fee loses to fresh, stronger evidence.
    CheapButStale,
    /// A reliable connector loses to a materially better fee profile.
    ReliableButExpensive,
    /// A newly observed connector receives explicit evidence risks.
    NewUnobserved,
    /// Equal quality routes use stable connector-id ordering.
    EqualScoreTieBreak,
    /// A multi-connector case guards the MVP route-score ordering against regressions.
    MixedRouteRegression,
}

impl ScenarioName {
    /// Returns a stable, human-readable fixture name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HealthyHighLiquidity => "healthy_high_liquidity",
            Self::LowLiquidityExcluded => "low_liquidity_excluded",
            Self::CheapButStale => "cheap_but_stale",
            Self::ReliableButExpensive => "reliable_but_expensive",
            Self::NewUnobserved => "new_unobserved",
            Self::EqualScoreTieBreak => "equal_score_tie_break",
            Self::MixedRouteRegression => "mixed_route_regression",
        }
    }
}

impl fmt::Display for ScenarioName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A local, reproducible connector with all facts needed by the ranker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulatedConnector {
    /// Stable connector identity.
    pub id: ConnectorId,
    /// Connector family represented by its generated hop.
    pub connector_type: ConnectorType,
    /// Adapter-reported generic capabilities.
    pub capabilities: ConnectorCapabilities,
    /// Directional liquidity returned in generated quotes.
    pub liquidity: Evidence<LiquidityInfo>,
    /// Fee returned in generated quotes.
    pub fee: Evidence<FeeQuote>,
    /// Hop reliability returned in generated quotes.
    pub reliability: Evidence<ReliabilityInfo>,
    /// Connector-level health, solvency, and historical observations.
    pub evidence: ConnectorEvidence,
}

impl SimulatedConnector {
    /// Generates the same one-hop quote for `amount` on every call.
    ///
    /// # Errors
    ///
    /// Returns an error only if the invariant that a simulated connector has
    /// one generated hop is violated.
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

/// Complete input and expected result for one deterministic routing scenario.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutingScenario {
    /// Fixture identity.
    pub name: ScenarioName,
    /// Payment submitted to every generated quote.
    pub request: PaymentRequest,
    /// Fixed evaluation timestamp.
    pub evaluated_at: EvidenceTimestamp,
    /// Fixed score configuration.
    pub config: RouteRankingConfig,
    /// Simulated connector fixtures, in deliberate input order.
    pub connectors: Vec<SimulatedConnector>,
    /// Expected ranked connector ids in order.
    pub expected_ranked: Vec<String>,
    /// Expected connector ids excluded before scoring.
    pub expected_rejected: Vec<String>,
}

impl RoutingScenario {
    /// Runs the fixture through the real deterministic ranker.
    ///
    /// # Errors
    ///
    /// Returns an error if a fixture cannot produce its declared one-hop quote.
    pub fn rank(&self) -> Result<RouteRanking, RouteCandidateError> {
        let candidates = self
            .connectors
            .iter()
            .map(|connector| connector.quote(self.request.amount))
            .collect::<Result<Vec<_>, _>>()?;
        let connector_evidence = self
            .connectors
            .iter()
            .map(|connector| (connector.id.clone(), connector.evidence.clone()))
            .collect::<BTreeMap<_, _>>();
        Ok(rank_routes(
            self.request,
            candidates,
            &connector_evidence,
            self.evaluated_at,
            self.config,
        ))
    }
}

/// Stable result of a scenario execution, suitable for a CLI or regression test.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioResult {
    /// Fixture identity.
    pub name: ScenarioName,
    /// Connector ids selected by the ranker, in order.
    pub ranked: Vec<String>,
    /// Connector ids rejected before scoring, in order.
    pub rejected: Vec<String>,
}

/// Failure while running or verifying a simulator fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioError {
    /// A fixture has an invalid connector identifier.
    InvalidConnectorId(ConnectorIdError),
    /// A fixture could not produce its quote.
    InvalidQuote(RouteCandidateError),
    /// The actual result diverged from the fixture's regression expectation.
    UnexpectedResult {
        /// Fixture whose result changed.
        name: ScenarioName,
        /// Expected ranked connector ids.
        expected_ranked: Vec<String>,
        /// Actual ranked connector ids.
        actual_ranked: Vec<String>,
        /// Expected rejected connector ids.
        expected_rejected: Vec<String>,
        /// Actual rejected connector ids.
        actual_rejected: Vec<String>,
    },
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConnectorId(error) => {
                write!(formatter, "invalid simulated connector: {error}")
            }
            Self::InvalidQuote(error) => write!(formatter, "invalid simulated quote: {error}"),
            Self::UnexpectedResult {
                name,
                expected_ranked,
                actual_ranked,
                expected_rejected,
                actual_rejected,
            } => write!(
                formatter,
                "scenario {name} changed: ranked {actual_ranked:?} (expected {expected_ranked:?}), rejected {actual_rejected:?} (expected {expected_rejected:?})"
            ),
        }
    }
}

impl Error for ScenarioError {}

const PAYMENT_AMOUNT: Amount = Amount::from_sats(10_000);
const FIRST_SEEN: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(1_000);
const EVALUATED_AT: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(5_000_000);

/// Returns every built-in scenario in a fixed order.
///
/// These fixtures contain no clock, RNG, or network input. They are intended to
/// be a compact regression suite for future scoring-policy changes.
///
/// # Errors
///
/// Returns an error if a built-in fixture contains an invalid connector id.
pub fn all_scenarios() -> Result<Vec<RoutingScenario>, ScenarioError> {
    Ok(vec![
        healthy_high_liquidity()?,
        low_liquidity_excluded()?,
        cheap_but_stale()?,
        reliable_but_expensive()?,
        new_unobserved()?,
        equal_score_tie_break()?,
        mixed_route_regression()?,
    ])
}

/// Runs every built-in scenario without checking its expected output.
///
/// # Errors
///
/// Returns an error if a fixture cannot create its generated quote.
pub fn run_all_scenarios() -> Result<Vec<ScenarioResult>, ScenarioError> {
    all_scenarios()?
        .into_iter()
        .map(|scenario| run_scenario(&scenario))
        .collect()
}

/// Runs every built-in scenario and verifies its expected stable output.
///
/// # Errors
///
/// Returns an error if a fixture is invalid or its result changes from the
/// checked-in regression expectation.
pub fn verify_all_scenarios() -> Result<Vec<ScenarioResult>, ScenarioError> {
    all_scenarios()?
        .into_iter()
        .map(|scenario| {
            let result = run_scenario(&scenario)?;
            if result.ranked != scenario.expected_ranked
                || result.rejected != scenario.expected_rejected
            {
                return Err(ScenarioError::UnexpectedResult {
                    name: scenario.name,
                    expected_ranked: scenario.expected_ranked,
                    actual_ranked: result.ranked,
                    expected_rejected: scenario.expected_rejected,
                    actual_rejected: result.rejected,
                });
            }
            Ok(result)
        })
        .collect()
}

fn run_scenario(scenario: &RoutingScenario) -> Result<ScenarioResult, ScenarioError> {
    let ranking = scenario.rank().map_err(ScenarioError::InvalidQuote)?;
    Ok(ScenarioResult {
        name: scenario.name,
        ranked: ranking
            .ranked
            .iter()
            .filter_map(first_connector_id)
            .collect(),
        rejected: ranking
            .rejected
            .iter()
            .filter_map(|route| route.candidate.hops.first())
            .map(|hop| hop.connector_id.to_string())
            .collect(),
    })
}

fn first_connector_id(route: &crate::routing::RankedRoute) -> Option<String> {
    route
        .candidate
        .hops
        .first()
        .map(|hop| hop.connector_id.to_string())
}

fn healthy_high_liquidity() -> Result<RoutingScenario, ScenarioError> {
    Ok(scenario(
        ScenarioName::HealthyHighLiquidity,
        vec![healthy_connector("cashu:healthy", 10)?],
        ["cashu:healthy"],
        [],
    ))
}

fn low_liquidity_excluded() -> Result<RoutingScenario, ScenarioError> {
    let mut low = healthy_connector("cashu:low-liquidity", 5)?;
    low.liquidity = fresh(LiquidityInfo::new(Amount::from_sats(9_999), None));
    Ok(scenario(
        ScenarioName::LowLiquidityExcluded,
        vec![low],
        [],
        ["cashu:low-liquidity"],
    ))
}

fn cheap_but_stale() -> Result<RoutingScenario, ScenarioError> {
    let healthy = healthy_connector("cashu:healthy", 10)?;
    let mut cheap = healthy_connector("cashu:cheap-stale", 1)?;
    cheap.liquidity = stale(LiquidityInfo::new(Amount::from_sats(20_000), None));
    cheap.reliability = stale(reliability(9_950, 500));
    cheap.evidence.health = stale(ConnectorHealth::Healthy);
    cheap.evidence.solvency = stale(SolvencyStatus::Supported);
    cheap.evidence.reliability = stale(reliability(9_950, 500));
    Ok(scenario(
        ScenarioName::CheapButStale,
        vec![cheap, healthy],
        ["cashu:healthy", "cashu:cheap-stale"],
        [],
    ))
}

fn reliable_but_expensive() -> Result<RoutingScenario, ScenarioError> {
    let healthy = healthy_connector("cashu:healthy", 10)?;
    let expensive = healthy_connector("cashu:reliable-expensive", 95)?;
    Ok(scenario(
        ScenarioName::ReliableButExpensive,
        vec![expensive, healthy],
        ["cashu:healthy", "cashu:reliable-expensive"],
        [],
    ))
}

fn new_unobserved() -> Result<RoutingScenario, ScenarioError> {
    let healthy = healthy_connector("cashu:healthy", 10)?;
    let mut new = healthy_connector("cashu:new", 5)?;
    new.evidence.first_observed_at = None;
    Ok(scenario(
        ScenarioName::NewUnobserved,
        vec![new, healthy],
        ["cashu:healthy", "cashu:new"],
        [],
    ))
}

fn equal_score_tie_break() -> Result<RoutingScenario, ScenarioError> {
    let alpha = healthy_connector("cashu:alpha", 10)?;
    let beta = healthy_connector("cashu:beta", 10)?;
    Ok(scenario(
        ScenarioName::EqualScoreTieBreak,
        vec![beta, alpha],
        ["cashu:alpha", "cashu:beta"],
        [],
    ))
}

fn mixed_route_regression() -> Result<RoutingScenario, ScenarioError> {
    let healthy = healthy_connector("cashu:healthy", 10)?;
    let mut cheap = healthy_connector("cashu:cheap-stale", 1)?;
    cheap.liquidity = stale(LiquidityInfo::new(Amount::from_sats(20_000), None));
    cheap.reliability = stale(reliability(9_950, 500));
    cheap.evidence.health = stale(ConnectorHealth::Healthy);
    cheap.evidence.solvency = stale(SolvencyStatus::Supported);
    cheap.evidence.reliability = stale(reliability(9_950, 500));
    let expensive = healthy_connector("cashu:reliable-expensive", 95)?;
    let mut new = healthy_connector("cashu:new", 5)?;
    new.evidence.first_observed_at = None;
    let mut low = healthy_connector("cashu:low-liquidity", 5)?;
    low.liquidity = fresh(LiquidityInfo::new(Amount::from_sats(9_999), None));
    Ok(scenario(
        ScenarioName::MixedRouteRegression,
        vec![cheap, low, expensive, new, healthy],
        [
            "cashu:healthy",
            "cashu:new",
            "cashu:reliable-expensive",
            "cashu:cheap-stale",
        ],
        ["cashu:low-liquidity"],
    ))
}

fn scenario<const RANKED: usize, const REJECTED: usize>(
    name: ScenarioName,
    connectors: Vec<SimulatedConnector>,
    expected_ranked: [&str; RANKED],
    expected_rejected: [&str; REJECTED],
) -> RoutingScenario {
    RoutingScenario {
        name,
        request: PaymentRequest::new(PAYMENT_AMOUNT),
        evaluated_at: EVALUATED_AT,
        config: RouteRankingConfig::default(),
        connectors,
        expected_ranked: expected_ranked.map(str::to_owned).to_vec(),
        expected_rejected: expected_rejected.map(str::to_owned).to_vec(),
    }
}

fn healthy_connector(id: &str, fee: u64) -> Result<SimulatedConnector, ScenarioError> {
    let id = ConnectorId::new(id).map_err(ScenarioError::InvalidConnectorId)?;
    let reliability = reliability(9_950, 500);
    Ok(SimulatedConnector {
        id: id.clone(),
        connector_type: ConnectorType::Cashu,
        capabilities: ConnectorCapabilities::new(true, true, true, true),
        liquidity: fresh(LiquidityInfo::new(Amount::from_sats(20_000), None)),
        fee: fresh(FeeQuote::new(Amount::from_sats(fee))),
        reliability: fresh(reliability),
        evidence: ConnectorEvidence::new(
            id,
            Some(FIRST_SEEN),
            fresh(ConnectorHealth::Healthy),
            fresh(SolvencyStatus::Supported),
            fresh(reliability),
        ),
    })
}

fn reliability(success_rate_basis_points: u16, observations: u64) -> ReliabilityInfo {
    ReliabilityInfo::new(success_rate_basis_points, observations)
        .expect("simulator reliability constants are valid")
}

fn fresh<T>(value: T) -> Evidence<T> {
    Evidence::reported(
        value,
        EvidenceSource::Observer,
        EVALUATED_AT,
        ConfidenceLevel::High,
    )
}

fn stale<T>(value: T) -> Evidence<T> {
    Evidence::reported_stale(
        value,
        EvidenceSource::Historical,
        FIRST_SEEN,
        ConfidenceLevel::Low,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_scenarios_match_their_checked_in_regression_results() {
        let results = verify_all_scenarios().expect("all scenario expectations should hold");
        assert_eq!(results.len(), 7);
    }

    #[test]
    fn scenario_runs_are_reproducible() {
        assert_eq!(
            run_all_scenarios().expect("first run"),
            run_all_scenarios().expect("second run")
        );
    }

    #[test]
    fn low_liquidity_is_excluded_before_ranking() {
        let scenario = all_scenarios()
            .expect("valid fixtures")
            .into_iter()
            .find(|scenario| scenario.name == ScenarioName::LowLiquidityExcluded)
            .expect("low liquidity fixture");
        let result = run_scenario(&scenario).expect("valid quote");

        assert!(result.ranked.is_empty());
        assert_eq!(result.rejected, vec!["cashu:low-liquidity"]);
    }
}
