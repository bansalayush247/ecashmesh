use core::fmt;
use std::error::Error;

/// A Bitcoin-denominated amount expressed in whole satoshis.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Amount(u64);

impl Amount {
    /// Zero satoshis.
    pub const ZERO: Self = Self(0);

    /// Creates an amount from whole satoshis.
    #[must_use]
    pub const fn from_sats(sats: u64) -> Self {
        Self(sats)
    }

    /// Returns the amount as whole satoshis.
    #[must_use]
    pub const fn sats(self) -> u64 {
        self.0
    }

    /// Adds two amounts.
    ///
    /// # Errors
    ///
    /// Returns [`AmountError::Overflow`] if the sum cannot be represented.
    pub fn checked_add(self, other: Self) -> Result<Self, AmountError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(AmountError::Overflow)
    }

    /// Subtracts two amounts.
    ///
    /// # Errors
    ///
    /// Returns [`AmountError::Underflow`] if the result would be negative.
    pub fn checked_sub(self, other: Self) -> Result<Self, AmountError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(AmountError::Underflow)
    }
}

impl From<u64> for Amount {
    fn from(sats: u64) -> Self {
        Self::from_sats(sats)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} sat", self.0)
    }
}

/// An arithmetic failure involving an [`Amount`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmountError {
    /// Addition exceeded the largest representable amount.
    Overflow,
    /// Subtraction would create a negative amount.
    Underflow,
}

impl fmt::Display for AmountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Overflow => "amount overflow",
            Self::Underflow => "amount underflow",
        })
    }
}

impl Error for AmountError {}

/// A stable, protocol-independent identifier for a connector.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConnectorId(String);

impl ConnectorId {
    /// Creates an identifier containing ASCII letters, digits, `-`, `_`, `.`, or `:`.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty, longer than 128 bytes, or
    /// contains a character outside the documented portable character set.
    pub fn new(value: impl Into<String>) -> Result<Self, ConnectorIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ConnectorIdError::Empty);
        }
        if value.len() > 128 {
            return Err(ConnectorIdError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return Err(ConnectorIdError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// Borrows the connector identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ConnectorId {
    type Error = ConnectorIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ConnectorId {
    type Error = ConnectorIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<str> for ConnectorId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ConnectorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An invalid [`ConnectorId`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectorIdError {
    /// Identifiers cannot be empty.
    Empty,
    /// Identifiers are limited to 128 bytes.
    TooLong,
    /// The identifier contains a character outside the portable identifier set.
    InvalidCharacter,
}

impl fmt::Display for ConnectorIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "connector identifier cannot be empty",
            Self::TooLong => "connector identifier cannot exceed 128 bytes",
            Self::InvalidCharacter => "connector identifier contains an invalid character",
        })
    }
}

impl Error for ConnectorIdError {}

/// The connector family represented by a route hop.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConnectorType {
    /// A Cashu mint or adapter.
    Cashu,
    /// A Fedimint federation or adapter.
    Fedimint,
    /// A Lightning node, gateway, or adapter.
    Lightning,
}

impl fmt::Display for ConnectorType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cashu => "Cashu",
            Self::Fedimint => "Fedimint",
            Self::Lightning => "Lightning",
        })
    }
}

/// Generic abilities an adapter reports for a connector.
///
/// These flags describe adapter-visible behavior, not protocol guarantees.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConnectorCapabilities {
    /// The connector can originate a payment or transfer.
    pub can_send: bool,
    /// The connector can accept a payment or transfer.
    pub can_receive: bool,
    /// The connector can exchange value with another connector.
    pub supports_cross_connector_transfer: bool,
    /// The connector can exchange value with Lightning.
    pub supports_lightning: bool,
}

impl ConnectorCapabilities {
    /// Creates a capability set from its individual flags.
    #[allow(clippy::fn_params_excessive_bools)]
    #[must_use]
    pub const fn new(
        can_send: bool,
        can_receive: bool,
        supports_cross_connector_transfer: bool,
        supports_lightning: bool,
    ) -> Self {
        Self {
            can_send,
            can_receive,
            supports_cross_connector_transfer,
            supports_lightning,
        }
    }

    /// Returns whether the connector can participate as a forwarding hop.
    #[must_use]
    pub const fn can_forward(self) -> bool {
        self.can_send && self.can_receive
    }
}

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

/// An observed fact, with explicit uncertainty and freshness states.
///
/// `Unknown` means no usable observation is available. `Stale` retains the
/// last observation so callers can explain what is out of date without treating
/// it as current evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Evidence<T> {
    /// A current observation.
    Known {
        /// The observed value.
        value: T,
        /// When the value was observed.
        observed_at: EvidenceTimestamp,
    },
    /// No usable observation is available.
    Unknown,
    /// A retained observation that must not be treated as current.
    Stale {
        /// The last observed value.
        value: T,
        /// When that value was observed.
        observed_at: EvidenceTimestamp,
    },
}

impl<T> Evidence<T> {
    /// Creates current evidence.
    #[must_use]
    pub const fn known(value: T, observed_at: EvidenceTimestamp) -> Self {
        Self::Known { value, observed_at }
    }

    /// Creates unknown evidence.
    #[must_use]
    pub const fn unknown() -> Self {
        Self::Unknown
    }

    /// Creates stale evidence while retaining the last observed value.
    #[must_use]
    pub const fn stale(value: T, observed_at: EvidenceTimestamp) -> Self {
        Self::Stale { value, observed_at }
    }

    /// Returns the observation value for known or stale evidence.
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Known { value, .. } | Self::Stale { value, .. } => Some(value),
            Self::Unknown => None,
        }
    }

    /// Returns the observation timestamp for known or stale evidence.
    #[must_use]
    pub const fn observed_at(&self) -> Option<EvidenceTimestamp> {
        match self {
            Self::Known { observed_at, .. } | Self::Stale { observed_at, .. } => Some(*observed_at),
            Self::Unknown => None,
        }
    }

    /// Returns whether the evidence is a current observation.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known { .. })
    }

    /// Returns whether the evidence is unavailable.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Returns whether the evidence is retained but stale.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
    }

    /// Maps an observed value while preserving its evidence state and timestamp.
    #[must_use]
    pub fn map<U>(self, transform: impl FnOnce(T) -> U) -> Evidence<U> {
        match self {
            Self::Known { value, observed_at } => Evidence::known(transform(value), observed_at),
            Self::Unknown => Evidence::Unknown,
            Self::Stale { value, observed_at } => Evidence::stale(transform(value), observed_at),
        }
    }
}

/// A connector's reported liquidity for a route amount.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiquidityInfo {
    /// The amount reported as currently available for this route direction.
    pub available: Amount,
    /// The greatest amount this observation can support, when the adapter knows it.
    pub maximum: Option<Amount>,
}

impl LiquidityInfo {
    /// Creates liquidity information.
    #[must_use]
    pub const fn new(available: Amount, maximum: Option<Amount>) -> Self {
        Self { available, maximum }
    }

    /// Returns whether the reported available liquidity covers `amount`.
    #[must_use]
    pub const fn can_cover(self, amount: Amount) -> bool {
        self.available.sats() >= amount.sats()
    }
}

/// A fee quoted by one route hop for the requested payment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeeQuote {
    /// The fee in satoshis.
    pub amount: Amount,
}

impl FeeQuote {
    /// Creates a fee quote.
    #[must_use]
    pub const fn new(amount: Amount) -> Self {
        Self { amount }
    }
}

/// Observed payment reliability for a connector or a route hop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReliabilityInfo {
    /// Success rate, expressed in basis points (`10_000` is 100%).
    pub success_rate_basis_points: u16,
    /// Number of completed observations behind the rate.
    pub observations: u64,
}

impl ReliabilityInfo {
    /// Creates reliability information if `success_rate_basis_points` is at most 100%.
    #[must_use]
    pub const fn new(success_rate_basis_points: u16, observations: u64) -> Option<Self> {
        if success_rate_basis_points <= 10_000 {
            Some(Self {
                success_rate_basis_points,
                observations,
            })
        } else {
            None
        }
    }
}

/// A factual concern that a caller may attach to a candidate or selected route.
///
/// This enum intentionally classifies concerns only; it does not assign a score
/// or decide whether a route should be selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RiskFactor {
    /// Reported liquidity cannot cover the requested amount.
    InsufficientLiquidity {
        /// Amount required by the route.
        required: Amount,
        /// Amount reported as available.
        available: Amount,
    },
    /// No usable liquidity observation is available.
    UnknownLiquidity,
    /// The available liquidity observation is stale.
    StaleLiquidity,
    /// No usable fee quote is available.
    UnknownFee,
    /// The available fee quote is stale.
    StaleFee,
    /// No usable reliability observation is available.
    UnknownReliability,
    /// The available reliability observation is stale.
    StaleReliability,
    /// Reliability is low according to a policy outside this crate.
    LowReliability {
        /// Observed success rate in basis points.
        success_rate_basis_points: u16,
    },
    /// A needed generic capability was not reported.
    MissingCapability {
        /// Name of the required capability.
        capability: &'static str,
    },
    /// An adapter supplied a domain-specific concern not yet modeled here.
    Other {
        /// Stable, human-readable classification.
        code: String,
    },
}

/// One connector traversal in a candidate route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteHop {
    /// Connector traversed by this hop.
    pub connector_id: ConnectorId,
    /// Family of the connector.
    pub connector_type: ConnectorType,
    /// Generic capabilities reported by the adapter.
    pub capabilities: ConnectorCapabilities,
    /// Observed directional liquidity.
    pub liquidity: Evidence<LiquidityInfo>,
    /// Fee quote for this hop.
    pub fee: Evidence<FeeQuote>,
    /// Observed reliability for this hop.
    pub reliability: Evidence<ReliabilityInfo>,
}

impl RouteHop {
    /// Creates a normalized route hop from adapter-provided facts.
    #[must_use]
    pub fn new(
        connector_id: ConnectorId,
        connector_type: ConnectorType,
        capabilities: ConnectorCapabilities,
        liquidity: Evidence<LiquidityInfo>,
        fee: Evidence<FeeQuote>,
        reliability: Evidence<ReliabilityInfo>,
    ) -> Self {
        Self {
            connector_id,
            connector_type,
            capabilities,
            liquidity,
            fee,
            reliability,
        }
    }
}

/// A discovered path for a requested amount, before it is selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCandidate {
    /// Amount the candidate is intended to deliver.
    pub amount: Amount,
    /// Ordered connector traversals from source to destination.
    pub hops: Vec<RouteHop>,
}

impl RouteCandidate {
    /// Creates a candidate with at least one hop.
    ///
    /// # Errors
    ///
    /// Returns [`RouteCandidateError::EmptyRoute`] when `hops` is empty.
    pub fn new(amount: Amount, hops: Vec<RouteHop>) -> Result<Self, RouteCandidateError> {
        if hops.is_empty() {
            return Err(RouteCandidateError::EmptyRoute);
        }
        Ok(Self { amount, hops })
    }

    /// Returns the number of connector traversals in the candidate.
    #[must_use]
    pub fn hop_count(&self) -> usize {
        self.hops.len()
    }
}

/// An invalid [`RouteCandidate`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteCandidateError {
    /// Routes must contain at least one hop.
    EmptyRoute,
}

impl fmt::Display for RouteCandidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("route candidate must contain at least one hop")
    }
}

impl Error for RouteCandidateError {}

/// Route-level facts produced by a future evaluation layer.
///
/// The core model stores these facts but intentionally does not derive them or
/// turn them into a score.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteQuality {
    /// Aggregate liquidity evidence for the complete route.
    pub liquidity: Evidence<LiquidityInfo>,
    /// Aggregate fee quote for the complete route.
    pub total_fee: Evidence<FeeQuote>,
    /// Aggregate reliability evidence for the complete route.
    pub reliability: Evidence<ReliabilityInfo>,
    /// Concerns identified by an evaluation policy.
    pub risk_factors: Vec<RiskFactor>,
}

impl RouteQuality {
    /// Creates route-level facts without applying a selection policy.
    #[must_use]
    pub fn new(
        liquidity: Evidence<LiquidityInfo>,
        total_fee: Evidence<FeeQuote>,
        reliability: Evidence<ReliabilityInfo>,
        risk_factors: Vec<RiskFactor>,
    ) -> Self {
        Self {
            liquidity,
            total_fee,
            reliability,
            risk_factors,
        }
    }
}

/// Human-readable context supplied with a route decision.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RouteExplanation {
    /// A short, user-facing account of the route.
    pub summary: String,
    /// Factual observations that support using the route.
    pub supporting_evidence: Vec<String>,
    /// Factual caveats that should be shown to the caller.
    pub caveats: Vec<String>,
}

impl RouteExplanation {
    /// Creates an explanation without implying any scoring semantics.
    #[must_use]
    pub fn new(
        summary: impl Into<String>,
        supporting_evidence: Vec<String>,
        caveats: Vec<String>,
    ) -> Self {
        Self {
            summary: summary.into(),
            supporting_evidence,
            caveats,
        }
    }
}

/// A route selected by a caller or a future routing layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    /// The discovered candidate that was selected.
    pub candidate: RouteCandidate,
    /// Route-level facts available at selection time.
    pub quality: RouteQuality,
    /// User-facing context for the selection.
    pub explanation: RouteExplanation,
}

impl Route {
    /// Combines an already-selected candidate with its facts and explanation.
    #[must_use]
    pub fn new(
        candidate: RouteCandidate,
        quality: RouteQuality,
        explanation: RouteExplanation,
    ) -> Self {
        Self {
            candidate,
            quality,
            explanation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBSERVED_AT: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(1_700_000_000);

    fn hop(connector_type: ConnectorType) -> RouteHop {
        RouteHop::new(
            ConnectorId::new("example:connector-1").expect("valid test identifier"),
            connector_type,
            ConnectorCapabilities::new(true, true, true, true),
            Evidence::known(LiquidityInfo::new(Amount::from(200_000), None), OBSERVED_AT),
            Evidence::known(FeeQuote::new(Amount::from(21)), OBSERVED_AT),
            Evidence::known(
                ReliabilityInfo::new(9_950, 1_000).expect("valid test reliability"),
                OBSERVED_AT,
            ),
        )
    }

    #[test]
    fn amount_is_satoshi_based_and_checked() {
        assert_eq!(Amount::from_sats(42).sats(), 42);
        assert_eq!(
            Amount::from_sats(1).checked_add(Amount::from_sats(2)),
            Ok(Amount::from_sats(3))
        );
        assert_eq!(
            Amount::ZERO.checked_sub(Amount::from_sats(1)),
            Err(AmountError::Underflow)
        );
        assert_eq!(
            Amount::from_sats(u64::MAX).checked_add(Amount::from_sats(1)),
            Err(AmountError::Overflow)
        );
        assert_eq!(Amount::from_sats(12).to_string(), "12 sat");
    }

    #[test]
    fn connector_ids_are_portable_and_validated() {
        let id = ConnectorId::new("cashu:mint_01.example").expect("portable connector id");
        assert_eq!(id.as_str(), "cashu:mint_01.example");
        assert_eq!(ConnectorId::new(""), Err(ConnectorIdError::Empty));
        assert_eq!(
            ConnectorId::new("contains spaces"),
            Err(ConnectorIdError::InvalidCharacter)
        );
        assert_eq!(
            ConnectorId::new("x".repeat(129)),
            Err(ConnectorIdError::TooLong)
        );
    }

    #[test]
    fn connector_types_cover_supported_protocols() {
        assert_eq!(ConnectorType::Cashu.to_string(), "Cashu");
        assert_eq!(ConnectorType::Fedimint.to_string(), "Fedimint");
        assert_eq!(ConnectorType::Lightning.to_string(), "Lightning");
    }

    #[test]
    fn capabilities_distinguish_forwarding_from_partial_support() {
        assert!(!ConnectorCapabilities::new(true, false, true, false).can_forward());
        assert!(ConnectorCapabilities::new(true, true, false, false).can_forward());
    }

    #[test]
    fn evidence_preserves_known_unknown_and_stale_states() {
        let known = Evidence::known("current", OBSERVED_AT);
        let unknown: Evidence<&str> = Evidence::unknown();
        let stale = Evidence::stale("last seen", OBSERVED_AT);

        assert!(known.is_known());
        assert_eq!(known.value(), Some(&"current"));
        assert_eq!(known.observed_at(), Some(OBSERVED_AT));
        assert!(unknown.is_unknown());
        assert_eq!(unknown.value(), None);
        assert_eq!(unknown.observed_at(), None);
        assert!(stale.is_stale());
        assert_eq!(stale.value(), Some(&"last seen"));
    }

    #[test]
    fn evidence_mapping_retains_the_state_and_timestamp() {
        let known = Evidence::known(21_u64, OBSERVED_AT).map(|value| value * 2);
        let stale = Evidence::stale(21_u64, OBSERVED_AT).map(|value| value * 2);
        let unknown: Evidence<u64> = Evidence::unknown();

        assert_eq!(known, Evidence::known(42, OBSERVED_AT));
        assert_eq!(stale, Evidence::stale(42, OBSERVED_AT));
        assert_eq!(unknown.map(|value| value * 2), Evidence::Unknown);
    }

    #[test]
    fn liquidity_reports_coverability_without_claiming_freshness() {
        let liquidity = LiquidityInfo::new(Amount::from(100), Some(Amount::from(150)));
        assert!(liquidity.can_cover(Amount::from(100)));
        assert!(!liquidity.can_cover(Amount::from(101)));
        assert_eq!(liquidity.maximum, Some(Amount::from(150)));
    }

    #[test]
    fn reliability_rejects_rates_above_one_hundred_percent() {
        assert_eq!(
            ReliabilityInfo::new(10_000, 1),
            Some(ReliabilityInfo {
                success_rate_basis_points: 10_000,
                observations: 1,
            })
        );
        assert_eq!(ReliabilityInfo::new(10_001, 1), None);
    }

    #[test]
    fn candidates_require_an_ordered_non_empty_path() {
        assert_eq!(
            RouteCandidate::new(Amount::from(1), vec![]),
            Err(RouteCandidateError::EmptyRoute)
        );

        let candidate = RouteCandidate::new(
            Amount::from(10_000),
            vec![hop(ConnectorType::Cashu), hop(ConnectorType::Lightning)],
        )
        .expect("route has hops");
        assert_eq!(candidate.hop_count(), 2);
        assert_eq!(candidate.hops[0].connector_type, ConnectorType::Cashu);
        assert_eq!(candidate.hops[1].connector_type, ConnectorType::Lightning);
    }

    #[test]
    fn a_route_keeps_quality_risks_and_explanation_separate() {
        let candidate =
            RouteCandidate::new(Amount::from(5_000), vec![hop(ConnectorType::Fedimint)])
                .expect("route has a hop");
        let quality = RouteQuality::new(
            Evidence::stale(LiquidityInfo::new(Amount::from(8_000), None), OBSERVED_AT),
            Evidence::known(FeeQuote::new(Amount::from(10)), OBSERVED_AT),
            Evidence::unknown(),
            vec![RiskFactor::StaleLiquidity, RiskFactor::UnknownReliability],
        );
        let explanation = RouteExplanation::new(
            "Uses a Fedimint connector",
            vec!["Quoted fee is current".into()],
            vec!["Liquidity is stale".into()],
        );
        let route = Route::new(candidate.clone(), quality.clone(), explanation.clone());

        assert_eq!(route.candidate, candidate);
        assert_eq!(route.quality, quality);
        assert_eq!(route.explanation, explanation);
        assert_eq!(
            route.quality.risk_factors,
            vec![RiskFactor::StaleLiquidity, RiskFactor::UnknownReliability]
        );
    }
}
