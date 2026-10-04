//! Deterministic acceptance for the 8-source model: four Cashu mints and four
//! Fedimint federations, ranked with the production MVP configuration.
//!
//! Every source starts fully and freshly observed so the weighted arithmetic is
//! visible (no penalty saturation); each experiment changes one evidence fact.

use std::collections::BTreeMap;

use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence, ConnectorHealth,
    ConnectorId, ConnectorType, Evidence, EvidenceSource, EvidenceTimestamp, FeeQuote,
    LiquidityInfo, PaymentRequest, RankedRoute, ReliabilityInfo, RiskFactor, RouteCandidate,
    RouteHop, RouteRanking, RouteRankingConfig, RouteRejectionReason, SolvencyStatus, rank_routes,
};

const NOW: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(10_000_000);
const EARLIER: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(9_000_000);
const FIRST_SEEN: EvidenceTimestamp = EvidenceTimestamp::from_unix_seconds(6_000_000);
const AMOUNT: Amount = Amount::from_sats(1_000);

/// Lab-shaped sources, cheapest first: fee `n` sats for the n-th source.
const SOURCES: [(&str, ConnectorType); 8] = [
    ("fedimint:fed-a", ConnectorType::Fedimint),
    ("fedimint:fed-b", ConnectorType::Fedimint),
    ("fedimint:fed-c", ConnectorType::Fedimint),
    ("fedimint:fed-d", ConnectorType::Fedimint),
    ("cashu:mint-a", ConnectorType::Cashu),
    ("cashu:mint-b", ConnectorType::Cashu),
    ("cashu:mint-c", ConnectorType::Cashu),
    ("cashu:mint-d", ConnectorType::Cashu),
];

struct Source {
    id: ConnectorId,
    connector_type: ConnectorType,
    liquidity: Evidence<LiquidityInfo>,
    fee: Evidence<FeeQuote>,
    reliability: Evidence<ReliabilityInfo>,
    evidence: ConnectorEvidence,
}

fn known<T>(value: T) -> Evidence<T> {
    Evidence::reported(value, EvidenceSource::Observer, NOW, ConfidenceLevel::High)
}

fn stale<T>(value: T) -> Evidence<T> {
    Evidence::reported_stale(
        value,
        EvidenceSource::Observer,
        EARLIER,
        ConfidenceLevel::High,
    )
}

fn reliability() -> ReliabilityInfo {
    ReliabilityInfo::new(9_900, 200).expect("valid reliability")
}

fn sources() -> Vec<Source> {
    SOURCES
        .iter()
        .zip(1_u64..)
        .map(|((id, connector_type), fee)| {
            let id = ConnectorId::new(*id).expect("valid id");
            Source {
                id: id.clone(),
                connector_type: *connector_type,
                liquidity: known(LiquidityInfo::new(Amount::from_sats(50_000), None)),
                fee: known(FeeQuote::new(Amount::from_sats(fee))),
                reliability: known(reliability()),
                evidence: ConnectorEvidence::new(
                    id,
                    Some(FIRST_SEEN),
                    known(ConnectorHealth::Healthy),
                    known(SolvencyStatus::Supported),
                    known(reliability()),
                ),
            }
        })
        .collect()
}

fn rank(sources: &[Source]) -> RouteRanking {
    let candidates = sources.iter().map(|source| {
        RouteCandidate::new(
            AMOUNT,
            vec![RouteHop::new(
                source.id.clone(),
                source.connector_type,
                ConnectorCapabilities::new(true, false, false, true),
                source.liquidity.clone(),
                source.fee.clone(),
                source.reliability.clone(),
            )],
        )
        .expect("valid candidate")
    });
    let evidence = sources
        .iter()
        .map(|source| (source.id.clone(), source.evidence.clone()))
        .collect::<BTreeMap<_, _>>();
    let ranking = rank_routes(
        PaymentRequest::new(AMOUNT),
        candidates,
        &evidence,
        NOW,
        RouteRankingConfig::default(),
    );
    for route in &ranking.ranked {
        assert_score_is_reconstructible(route);
    }
    ranking
}

fn order(ranking: &RouteRanking) -> Vec<&str> {
    ranking
        .ranked
        .iter()
        .map(|route| route.candidate.hops[0].connector_id.as_str())
        .collect()
}

fn route<'a>(ranking: &'a RouteRanking, id: &str) -> &'a RankedRoute {
    ranking
        .ranked
        .iter()
        .find(|route| route.candidate.hops[0].connector_id.as_str() == id)
        .expect("source is ranked")
}

fn index(sources: &[Source], id: &str) -> usize {
    sources
        .iter()
        .position(|source| source.id.as_str() == id)
        .expect("fixture source")
}

/// The final score must follow exactly from the configured weights and the
/// configured penalty for each reported risk.
fn assert_score_is_reconstructible(route: &RankedRoute) {
    let config = RouteRankingConfig::default();
    let w = config.weights;
    let s = route.signals;
    let weighted = [
        (s.liquidity_confidence, w.liquidity_confidence),
        (s.reliability, w.reliability),
        (s.fee_reasonableness, w.fee_reasonableness),
        (s.freshness, w.freshness),
        (s.solvency_confidence, w.solvency_confidence),
        (s.historical_behavior, w.historical_behavior),
    ];
    let total_weight = weighted
        .iter()
        .map(|(_, weight)| u32::from(*weight))
        .sum::<u32>();
    let base = weighted
        .iter()
        .map(|(signal, weight)| u32::from(*signal) * u32::from(*weight))
        .sum::<u32>()
        / total_weight;
    assert_eq!(u32::from(route.base_score), base);
    let penalty = route
        .quality
        .risk_factors
        .iter()
        .map(|risk| u32::from(config.risk_penalties.for_risk(risk)))
        .sum::<u32>()
        .min(10_000);
    assert_eq!(u32::from(route.risk_penalty), penalty);
    assert_eq!(
        route.score,
        route.base_score.saturating_sub(route.risk_penalty)
    );
}

#[test]
fn mvp_weights_are_preserved() {
    let weights = RouteRankingConfig::default().weights;
    assert_eq!(
        [
            weights.liquidity_confidence,
            weights.reliability,
            weights.fee_reasonableness,
            weights.freshness,
            weights.solvency_confidence,
            weights.historical_behavior,
        ],
        [25, 20, 15, 15, 15, 10]
    );
}

#[test]
fn all_eight_sources_are_ranked_with_exact_weighted_scores() {
    let ranking = rank(&sources());
    assert!(ranking.rejected.is_empty());
    assert_eq!(order(&ranking), SOURCES.map(|(id, _)| id).to_vec());
    let cashu = ranking
        .ranked
        .iter()
        .filter(|route| route.candidate.hops[0].connector_type == ConnectorType::Cashu)
        .count();
    assert_eq!((cashu, ranking.ranked.len() - cashu), (4, 4));
    // Fully observed: liquidity 10000, reliability 9900, freshness, solvency
    // and history 10000. A fee of n sats on 1000 is 10n bp of a 100 bp bound,
    // so fee reasonableness is 10000 - 1000n and each sat costs 150 points.
    let scores = ranking
        .ranked
        .iter()
        .map(|route| route.score)
        .collect::<Vec<_>>();
    assert_eq!(scores, [9830, 9680, 9530, 9380, 9230, 9080, 8930, 8780]);
    assert!(ranking.ranked.iter().all(|route| route.risk_penalty == 0));
}

#[test]
fn experiment_a_a_materially_worse_fee_lowers_score_and_rank() {
    let mut sources = sources();
    let before = rank(&sources);
    let i = index(&sources, "fedimint:fed-a");
    sources[i].fee = known(FeeQuote::new(Amount::from_sats(9)));
    let after = rank(&sources);

    let (old, new) = (
        route(&before, "fedimint:fed-a"),
        route(&after, "fedimint:fed-a"),
    );
    assert_eq!(
        (
            old.signals.fee_reasonableness,
            new.signals.fee_reasonableness
        ),
        (9_000, 1_000)
    );
    // 15% of an 8000-point fee-signal drop.
    assert_eq!((old.score, new.score), (9830, 8630));
    assert_eq!(order(&before)[0], "fedimint:fed-a");
    assert_eq!(order(&after).last(), Some(&"fedimint:fed-a"));
}

#[test]
fn experiment_b_funding_below_the_required_amount_rejects_the_source() {
    let mut sources = sources();
    let i = index(&sources, "fedimint:fed-b");
    // Shrinking but still positive headroom is not a graded score signal:
    // liquidity confidence reflects evidence quality, not surplus size.
    sources[i].liquidity = known(LiquidityInfo::new(Amount::from_sats(1_000), None));
    let thin = rank(&sources);
    assert_eq!(route(&thin, "fedimint:fed-b").score, 9680);

    sources[i].liquidity = known(LiquidityInfo::new(Amount::from_sats(999), None));
    let short = rank(&sources);
    assert_eq!(short.ranked.len(), 7);
    assert!(!order(&short).contains(&"fedimint:fed-b"));
    assert_eq!(
        short.rejected[0].reasons,
        [RouteRejectionReason::InsufficientLiquidity {
            connector_id: sources[i].id.clone(),
            required: AMOUNT,
            available: Amount::from_sats(999),
        }]
    );
}

#[test]
fn experiment_c_an_unavailable_gateway_removes_reliability_credit() {
    let mut sources = sources();
    let i = index(&sources, "fedimint:fed-c");
    // A Fedimint connector is only as reachable as its Lightning gateway.
    sources[i].evidence.health = known(ConnectorHealth::Unavailable);
    let after = rank(&sources);
    let fed_c = route(&after, "fedimint:fed-c");
    assert_eq!(fed_c.signals.reliability, 0);
    // 20% of the 9900-point reliability signal.
    assert_eq!(fed_c.score, 9530 - 1980);
    assert_eq!(order(&after).last(), Some(&"fedimint:fed-c"));
}

#[test]
fn experiment_d_stale_evidence_lowers_freshness_and_adds_penalties() {
    let mut sources = sources();
    let before = rank(&sources);
    let i = index(&sources, "fedimint:fed-d");
    sources[i].liquidity = stale(LiquidityInfo::new(Amount::from_sats(50_000), None));
    sources[i].fee = stale(FeeQuote::new(Amount::from_sats(4)));
    sources[i].reliability = stale(reliability());
    sources[i].evidence.health = stale(ConnectorHealth::Healthy);
    let after = rank(&sources);

    let (old, new) = (
        route(&before, "fedimint:fed-d"),
        route(&after, "fedimint:fed-d"),
    );
    assert_eq!(old.signals.freshness, 10_000);
    // Four of six freshness inputs are stale (2500 each).
    assert_eq!(new.signals.freshness, 5_000);
    let stale_risks = new
        .quality
        .risk_factors
        .iter()
        .filter(|risk| {
            matches!(
                risk,
                RiskFactor::StaleLiquidity
                    | RiskFactor::StaleFee
                    | RiskFactor::StaleReliability
                    | RiskFactor::StaleEvidence { .. }
            )
        })
        .count();
    assert_eq!(stale_risks, 4);
    assert_eq!(new.risk_penalty, 4 * 500);
    assert!(new.score < old.score);
    assert_eq!(order(&after).last(), Some(&"fedimint:fed-d"));
}

#[test]
fn unknown_evidence_stays_unknown_and_is_never_treated_as_zero_or_success() {
    let mut sources = sources();
    let i = index(&sources, "cashu:mint-a");
    sources[i].liquidity = Evidence::Unknown;
    sources[i].reliability = Evidence::Unknown;
    sources[i].evidence.solvency = Evidence::Unknown;
    sources[i].evidence.reliability = Evidence::Unknown;
    let ranking = rank(&sources);

    // Unknown liquidity is not zero liquidity: no insufficient-funds rejection.
    assert!(ranking.rejected.is_empty());
    let mint_a = route(&ranking, "cashu:mint-a");
    // ...and it earns no confidence it does not have.
    assert_eq!(mint_a.signals.liquidity_confidence, 0);
    assert_eq!(mint_a.signals.reliability, 0);
    assert_eq!(mint_a.signals.solvency_confidence, 0);
    let risks = &mint_a.quality.risk_factors;
    assert!(risks.contains(&RiskFactor::UnknownLiquidity));
    assert!(risks.contains(&RiskFactor::UnknownSolvency));
    assert!(risks.contains(&RiskFactor::UnknownReliability));
    assert!(mint_a.quality.liquidity.value().is_none());
    assert!(mint_a.quality.reliability.value().is_none());
}

#[test]
fn liquidity_evidence_moves_its_25_percent_contribution_and_the_rank() {
    // The liquidity signal is the evidence confidence: High 10000, Medium
    // 6000, halved when stale, 0 when unknown. Liquidity is also one of six
    // freshness inputs, so stale/unknown evidence lowers freshness too.
    let cases = [
        // (evidence, liquidity, freshness, base, penalty, rank)
        (
            known(LiquidityInfo::new(AMOUNT, None)),
            10_000,
            10_000,
            9830,
            0,
            1,
        ),
        (
            Evidence::reported(
                LiquidityInfo::new(AMOUNT, None),
                EvidenceSource::Observer,
                NOW,
                ConfidenceLevel::Medium,
            ),
            6_000,
            10_000,
            8830,
            0,
            7,
        ),
        // Stale: one of six freshness inputs at 2500 -> 8750; stale_liquidity.
        (
            stale(LiquidityInfo::new(AMOUNT, None)),
            5_000,
            8_750,
            8392,
            500,
            8,
        ),
        // Unknown: freshness input 0 -> 8333; unknown_liquidity.
        (Evidence::Unknown, 0, 8_333, 7079, 800, 8),
    ];
    for (liquidity, signal, freshness, base, penalty, position) in cases {
        let mut sources = sources();
        let i = index(&sources, "fedimint:fed-a");
        sources[i].liquidity = liquidity;
        let ranking = rank(&sources);
        let fed_a = route(&ranking, "fedimint:fed-a");
        assert_eq!(fed_a.signals.liquidity_confidence, signal);
        assert_eq!(fed_a.signals.freshness, freshness);
        // 25% liquidity + 15% freshness are the only inputs that moved.
        let expected = (u32::from(signal) * 25
            + 9_900 * 20
            + 9_000 * 15
            + u32::from(freshness) * 15
            + 10_000 * 15
            + 10_000 * 10)
            / 100;
        assert_eq!(u32::from(fed_a.base_score), expected);
        assert_eq!(fed_a.base_score, base);
        assert_eq!(fed_a.risk_penalty, penalty);
        assert_eq!(fed_a.score, base - penalty);
        assert_eq!(
            order(&ranking)
                .iter()
                .position(|id| *id == "fedimint:fed-a"),
            Some(position - 1)
        );
    }
}
