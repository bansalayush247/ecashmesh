//! Routing-fee feasibility: can the paying node afford the measured route?
//!
//! A source's Lightning node pays the network's routing fee out of a budget
//! fixed before it dispatches the payment, and fails any route that costs more:
//!
//! - Fedimint `LNv2` gateway: its send state machine sets `max_fee` to the
//!   contract amount minus its `min_contract_amount`, i.e. the client's send fee
//!   minus the gateway's minimum send fee (`gateway_routing_fee_budget_msat`
//!   in the bridge quote, from the gateway's native routing info).
//! - Cashu (CDK) mint: a melt pays with `max_fee_amount = fee_reserve` of the
//!   melt quote, enforced by its LND backend as a fixed msat fee limit.
//!
//! The budget is what the paying node may spend on routing; it is not the fee
//! the payer is charged, and the two are never compared. The measured route fee
//! comes from the regtest lab's liquidity evidence for the same node. A source
//! is excluded only on fresh evidence that the route costs more than the
//! budget; missing evidence leaves feasibility unknown, as for liquidity.

use ecashmesh_core::Amount;
use serde::Serialize;

use crate::probe::{ChannelState, LiquidityEvidence, ProbeMethod, ProbeOutcome};

/// Issue code of a source excluded for an unaffordable route.
pub(crate) const INFEASIBLE_FEE_BUDGET: &str = "INFEASIBLE_FEE_BUDGET";

/// What the source's node must pay to route this payment, from lab evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteFee {
    /// The fee of a route that reached the payee.
    Exact {
        msat: u64,
        basis: RouteFeeBasis,
    },
    /// The route was measured only part of the way (a route-hint hop, or a
    /// relay whose own forwarding fee is unknown): the full fee is higher.
    AtLeast {
        msat: u64,
        basis: RouteFeeBasis,
    },
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RouteFeeBasis {
    /// A probe from the source's own node.
    LightningProbe,
    /// The relay's probe plus the relay's own forwarding fee.
    RelayedProbe,
    /// An active channel straight to the payee covers the amount: no hop
    /// in between charges a fee.
    DirectChannel,
    /// The payee is the selected gateway's own node: no Lightning payment.
    GatewayIsPayee,
}

/// The recorded verdict for one source. Contains no credentials.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct FeeBudgetCheck {
    pub source_id: String,
    /// Fedimint: the gateway's Lightning routing-fee budget; Cashu: the melt
    /// quote's fee reserve.
    pub budget_kind: &'static str,
    pub probe_fee_msat: Option<u64>,
    /// `exact` or `lower_bound`.
    pub probe_fee_bound: Option<&'static str>,
    pub probe_fee_basis: Option<RouteFeeBasis>,
    pub fee_budget_msat: Option<u64>,
    /// `None`: not determined; the source is ranked as before.
    pub feasible: Option<bool>,
    pub reason: &'static str,
}

impl FeeBudgetCheck {
    pub(crate) fn infeasible(&self) -> bool {
        self.feasible == Some(false)
    }

    pub(crate) fn message(&self) -> String {
        let fee = self.probe_fee_msat.unwrap_or_default();
        let budget = self.fee_budget_msat.unwrap_or_default();
        let at_least = if self.probe_fee_bound == Some("lower_bound") {
            "at least "
        } else {
            ""
        };
        match self.budget_kind {
            "gateway_routing_fee_budget" => format!(
                "Measured Lightning route fee {at_least}{fee} msat exceeds the selected gateway's routing-fee budget of {budget} msat; the gateway would refuse this route"
            ),
            _ => format!(
                "Measured Lightning route fee {at_least}{fee} msat exceeds the melt quote's fee reserve of {budget} msat; the mint would refuse this route"
            ),
        }
    }
}

/// The routing fee the source's node would pay, from evidence already bound
/// to that node (`apply_liquidity` drops evidence from any other node). Only
/// fresh evidence for exactly this amount counts.
pub(crate) fn route_fee(
    probe: Option<&LiquidityEvidence>,
    channels: Option<&ChannelState>,
    payee: Option<&str>,
    gateway: Option<&str>,
    amount: Amount,
    now: u64,
) -> RouteFee {
    if payee.is_some() && payee == gateway {
        return RouteFee::Exact {
            msat: 0,
            basis: RouteFeeBasis::GatewayIsPayee,
        };
    }
    if let Some(probe) = probe.filter(|probe| {
        probe.outcome == ProbeOutcome::Routable
            && probe.freshness(now) == "fresh"
            && probe.probed_amount_sats == amount.sats()
    }) && let Some(fee) = probe.routing_fee_msat
    {
        return match probe.evidence_source {
            ProbeMethod::LightningProbe if probe.reached_destination == Some(true) => {
                RouteFee::Exact {
                    msat: fee,
                    basis: RouteFeeBasis::LightningProbe,
                }
            }
            ProbeMethod::LightningProbe => RouteFee::AtLeast {
                msat: fee,
                basis: RouteFeeBasis::LightningProbe,
            },
            ProbeMethod::RelayedProbe => match probe.relay_hop_fee_msat {
                Some(hop) if probe.reached_destination == Some(true) => RouteFee::Exact {
                    msat: fee.saturating_add(hop),
                    basis: RouteFeeBasis::RelayedProbe,
                },
                hop => RouteFee::AtLeast {
                    msat: fee.saturating_add(hop.unwrap_or_default()),
                    basis: RouteFeeBasis::RelayedProbe,
                },
            },
        };
    }
    if let Some(channels) = channels.filter(|channels| channels.freshness(now) == "fresh")
        && channels
            .payee_direct_outbound_sats
            .is_some_and(|direct| direct >= amount.sats())
    {
        return RouteFee::Exact {
            msat: 0,
            basis: RouteFeeBasis::DirectChannel,
        };
    }
    RouteFee::Unknown
}

/// `feasible` only when the exact route fee fits the budget; infeasible when
/// even a lower bound exceeds it; otherwise undetermined.
pub(crate) fn check(
    source_id: &str,
    budget_kind: &'static str,
    route_fee: RouteFee,
    budget_msat: Option<u64>,
) -> FeeBudgetCheck {
    let (probe_fee_msat, probe_fee_bound, probe_fee_basis) = match route_fee {
        RouteFee::Exact { msat, basis } => (Some(msat), Some("exact"), Some(basis)),
        RouteFee::AtLeast { msat, basis } => (Some(msat), Some("lower_bound"), Some(basis)),
        RouteFee::Unknown => (None, None, None),
    };
    let (feasible, reason) = match (route_fee, budget_msat) {
        (_, None) => (None, "FEE_BUDGET_UNKNOWN"),
        (RouteFee::Unknown, Some(_)) => (None, "PROBE_FEE_UNKNOWN"),
        (RouteFee::Exact { msat, .. } | RouteFee::AtLeast { msat, .. }, Some(budget))
            if msat > budget =>
        {
            (Some(false), INFEASIBLE_FEE_BUDGET)
        }
        (RouteFee::Exact { .. }, Some(_)) => (Some(true), "WITHIN_FEE_BUDGET"),
        (RouteFee::AtLeast { .. }, Some(_)) => (None, "PROBE_FEE_LOWER_BOUND_WITHIN_BUDGET"),
    };
    FeeBudgetCheck {
        source_id: source_id.to_owned(),
        budget_kind,
        probe_fee_msat,
        probe_fee_bound,
        probe_fee_basis,
        fee_budget_msat: budget_msat,
        feasible,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ChannelStateMethod;

    const NOW: u64 = 1_000;
    const AMOUNT: u64 = 1_000;

    fn exact(fee: u64, budget: u64) -> FeeBudgetCheck {
        check(
            "fedimint:a",
            "gateway_routing_fee_budget",
            RouteFee::Exact {
                msat: fee,
                basis: RouteFeeBasis::LightningProbe,
            },
            Some(budget),
        )
    }

    #[test]
    fn zero_budget_cannot_pay_any_routing_fee() {
        let verdict = exact(1, 0);
        assert_eq!(verdict.feasible, Some(false));
        assert_eq!(verdict.reason, INFEASIBLE_FEE_BUDGET);
        assert!(verdict.infeasible());
        assert_eq!(
            verdict.message(),
            "Measured Lightning route fee 1 msat exceeds the selected gateway's routing-fee budget of 0 msat; the gateway would refuse this route"
        );
        // A route without intermediate hops costs nothing and fits.
        assert_eq!(exact(0, 0).feasible, Some(true));
    }

    #[test]
    fn a_fee_equal_to_or_under_the_budget_is_feasible() {
        for fee in [4_000, 3_999] {
            let verdict = exact(fee, 4_000);
            assert_eq!(verdict.feasible, Some(true), "{fee}");
            assert_eq!(verdict.reason, "WITHIN_FEE_BUDGET");
            assert_eq!(verdict.probe_fee_bound, Some("exact"));
        }
        assert_eq!(exact(4_001, 4_000).feasible, Some(false));
    }

    #[test]
    fn lower_bounds_only_ever_prove_infeasibility() {
        let at_least = |msat| RouteFee::AtLeast {
            msat,
            basis: RouteFeeBasis::RelayedProbe,
        };
        let over = check(
            "x",
            "gateway_routing_fee_budget",
            at_least(5_000),
            Some(4_000),
        );
        assert_eq!(over.feasible, Some(false));
        assert_eq!(over.probe_fee_bound, Some("lower_bound"));
        assert!(over.message().contains("at least 5000 msat"));
        let under = check(
            "x",
            "gateway_routing_fee_budget",
            at_least(3_000),
            Some(4_000),
        );
        assert_eq!(under.feasible, None);
        assert_eq!(under.reason, "PROBE_FEE_LOWER_BOUND_WITHIN_BUDGET");
    }

    #[test]
    fn missing_evidence_leaves_feasibility_undetermined() {
        let unknown_fee = check("x", "melt_fee_reserve", RouteFee::Unknown, Some(0));
        assert_eq!(
            (unknown_fee.feasible, unknown_fee.reason),
            (None, "PROBE_FEE_UNKNOWN")
        );
        let unknown_budget = check(
            "x",
            "gateway_routing_fee_budget",
            RouteFee::Exact {
                msat: 1_000_000,
                basis: RouteFeeBasis::LightningProbe,
            },
            None,
        );
        assert_eq!(
            (unknown_budget.feasible, unknown_budget.reason),
            (None, "FEE_BUDGET_UNKNOWN")
        );
    }

    #[test]
    fn cashu_budget_is_the_melt_fee_reserve() {
        let verdict = check(
            "cashu:mint-a",
            "melt_fee_reserve",
            RouteFee::Exact {
                msat: 2_002,
                basis: RouteFeeBasis::LightningProbe,
            },
            Some(2_000),
        );
        assert!(verdict.infeasible());
        assert!(
            verdict
                .message()
                .contains("melt quote's fee reserve of 2000 msat")
        );
    }

    fn probe(method: ProbeMethod, fee: u64, reached: bool) -> LiquidityEvidence {
        let mut probe = LiquidityEvidence::new("fedimint:b", method, "gateway-B", AMOUNT, NOW);
        probe.outcome = ProbeOutcome::Routable;
        probe.routing_fee_msat = Some(fee);
        probe.reached_destination = Some(reached);
        probe
    }

    fn fee_of(probe: &LiquidityEvidence) -> RouteFee {
        route_fee(
            Some(probe),
            None,
            Some("payee"),
            None,
            Amount::from_sats(AMOUNT),
            NOW,
        )
    }

    #[test]
    fn route_fee_is_the_fee_the_paying_node_pays() {
        let basis = RouteFeeBasis::LightningProbe;
        let own = probe(ProbeMethod::LightningProbe, 2_002, true);
        assert_eq!(fee_of(&own), RouteFee::Exact { msat: 2_002, basis });
        // Probed only up to a route-hint hop: the last hops cost more.
        let hinted = probe(ProbeMethod::LightningProbe, 1_001, false);
        assert_eq!(fee_of(&hinted), RouteFee::AtLeast { msat: 1_001, basis });

        // A relay's probe excludes the relay's own forwarding fee, which the
        // gateway also pays.
        let basis = RouteFeeBasis::RelayedProbe;
        let mut relayed = probe(ProbeMethod::RelayedProbe, 1_001, true);
        assert_eq!(fee_of(&relayed), RouteFee::AtLeast { msat: 1_001, basis });
        relayed.relay_hop_fee_msat = Some(1_001);
        assert_eq!(fee_of(&relayed), RouteFee::Exact { msat: 2_002, basis });
    }

    #[test]
    fn only_fresh_conclusive_evidence_for_this_amount_measures_a_fee() {
        let amount = Amount::from_sats(AMOUNT);
        let mut stale = probe(ProbeMethod::LightningProbe, 0, true);
        stale.observed_at_unix_seconds = NOW - 60;
        let mut other_amount = probe(ProbeMethod::LightningProbe, 0, true);
        other_amount.probed_amount_sats = AMOUNT * 2;
        let mut failed = probe(ProbeMethod::LightningProbe, 0, true);
        failed.outcome = ProbeOutcome::Unknown;
        for evidence in [stale, other_amount, failed] {
            assert_eq!(
                route_fee(Some(&evidence), None, Some("payee"), None, amount, NOW),
                RouteFee::Unknown
            );
        }
    }

    #[test]
    fn direct_channels_and_own_invoices_need_no_routing_fee() {
        let amount = Amount::from_sats(AMOUNT);
        let mut channels = ChannelState::new(
            "fedimint:b",
            ChannelStateMethod::GatewayChannelState,
            "gateway-B",
            NOW,
        );
        channels.reachable = true;
        channels.payee_direct_outbound_sats = Some(AMOUNT);
        assert_eq!(
            route_fee(None, Some(&channels), Some("payee"), None, amount, NOW),
            RouteFee::Exact {
                msat: 0,
                basis: RouteFeeBasis::DirectChannel
            }
        );
        channels.payee_direct_outbound_sats = Some(AMOUNT - 1);
        assert_eq!(
            route_fee(None, Some(&channels), Some("payee"), None, amount, NOW),
            RouteFee::Unknown
        );
        assert_eq!(
            route_fee(None, None, Some("gw"), Some("gw"), amount, NOW),
            RouteFee::Exact {
                msat: 0,
                basis: RouteFeeBasis::GatewayIsPayee
            }
        );
    }
}
