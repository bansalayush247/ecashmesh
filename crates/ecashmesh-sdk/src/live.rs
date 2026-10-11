//! Quote-backed live route construction.
//!
//! This module is intentionally a thin composition layer: Cashu protocol I/O
//! and normalization remain in `ecashmesh-cashu`; bounded route generation and
//! ranking remain in `ecashmesh-core`. It never creates a mint-to-mint edge.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ecashmesh_cashu::{
    CashuObservation, CashuPaymentRequest, KeysetFee, MintQuote, QuoteObservation,
    discovery::DiscoveryService,
};
use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorHealth, ConnectorId, ConnectorSnapshot, ConnectorType,
    Evidence, EvidenceSource, EvidenceTimestamp, LightningInvoice, PaymentRequest, RouteRanking,
    RouteRankingConfig, rank_routes,
};
use ecashmesh_fedimint::{FederationObservation, FedimintQuote, FedimintService};
use serde_json::{Value, json};

use crate::fee_budget::{self, FeeBudgetCheck, RouteFee};
use crate::liquidity::{
    ChannelState, LiquidityBasis, LiquidityEvidence, ProbeEffect, SourceLiquidity,
};
use crate::sources::{ConnectorBatch, unix_now};
use crate::{LiquidityOracle, PaymentHistory};

/// A destination normalized by backend code before discovery or quote requests.
#[derive(Clone, Debug)]
pub(super) enum LiveDestination {
    Lightning(LightningInvoice),
    Cashu(CashuPaymentRequest),
}

impl LiveDestination {
    pub(super) fn parse(kind: &str, value: &str) -> Result<Self, String> {
        match kind {
            "lightning" => LightningInvoice::parse(value)
                .map(Self::Lightning)
                .map_err(|error| error.to_string()),
            "cashu" => CashuPaymentRequest::parse(value)
                .map(Self::Cashu)
                .map_err(|error| error.to_string()),
            _ => Err("destination type must be lightning or cashu in live mode".into()),
        }
    }

    pub(super) fn destination_mint_urls(&self) -> Vec<&str> {
        match self {
            Self::Lightning(_) => Vec::new(),
            Self::Cashu(request) => request.mint_urls.iter().map(String::as_str).collect(),
        }
    }

    fn type_code(&self) -> &'static str {
        match self {
            Self::Lightning(_) => "lightning",
            Self::Cashu(_) => "cashu",
        }
    }
}

/// A successful, quote-backed ranking plus data for API inspection.
pub(super) struct LiveEvaluation {
    pub ranking: RouteRanking,
    pub connectors: Vec<ConnectorSnapshot>,
    pub quote_observations: Vec<Value>,
    /// Fee terms retained separately from generic route quality evidence.
    pub fee_terms: Vec<LiveFeeTerms>,
    pub expires_at_unix_seconds: u64,
    pub graph_context: Value,
    /// Verified gateway-fee comparisons that lack a native federation quote.
    /// They are intentionally kept outside the executable route ranking.
    pub gateway_estimated_sources: Vec<Value>,
}

/// Quote and keyset-fee facts for a quoted source connector.
pub(super) enum LiveFeeTerms {
    /// NUT-05 upper-bound reserve, not a final paid fee.
    Cashu {
        source_connector: ConnectorId,
        fee_reserve_sats: Amount,
        input_fees: Evidence<Vec<KeysetFee>>,
    },
    /// Explicit Fedimint fee components from a non-mutating host quote.
    Fedimint {
        source_connector: ConnectorId,
        total_fee_sats: Amount,
        federation_fee_sats: Amount,
        gateway_fee_sats: Option<Amount>,
        destination_fee_sats: Option<Amount>,
    },
}

/// A real-data failure that deliberately does not manufacture a candidate.
pub(super) struct LiveNoRoute {
    pub details: Vec<String>,
    pub quote_observations: Vec<Value>,
}

/// Builds only declared Cashu-to-Lightning edges for the normalized destination
/// and ranks one candidate per feasible source.
#[allow(clippy::too_many_lines)] // Keeps the full live feasibility chain and its exact no-route diagnostics together.
#[allow(clippy::too_many_arguments)] // Each optional evidence source is an explicit input.
pub(super) async fn evaluate(
    service: &Arc<DiscoveryService>,
    fedimint: &Arc<FedimintService>,
    probes: Option<&dyn LiquidityOracle>,
    history: Option<&dyn PaymentHistory>,
    batch: &ConnectorBatch,
    sources: Vec<ConnectorSnapshot>,
    destination: LiveDestination,
    amount: Amount,
    balances: &BTreeMap<String, u64>,
) -> Result<LiveEvaluation, LiveNoRoute> {
    let mut quote_observations = Vec::new();
    let destination_type = destination.type_code();
    let ResolvedDestination {
        invoice,
        context: destination_context,
        expiry: destination_expiry,
    } = match resolve_destination(service, batch, destination, amount, &mut quote_observations)
        .await
    {
        Ok(resolved) => resolved,
        Err(details) => {
            return Err(LiveNoRoute {
                details,
                quote_observations,
            });
        }
    };

    let by_id = batch
        .live_observations
        .iter()
        .map(|observation| (observation.id.clone(), observation))
        .collect::<BTreeMap<_, _>>();
    let mut live_sources = Vec::new();
    let mut live_fee_terms = Vec::new();
    let mut unavailable = BTreeSet::new();
    let mut expiries = Vec::new();
    let mut no_route_details = Vec::new();
    let mut fee_checks = BTreeMap::new();
    let mut gateway_estimated_sources = Vec::new();
    if let Some(expiry) = destination_expiry {
        expiries.push(expiry);
    }
    // Poll independent source reads together. Preserve input ordering when
    // collecting results so completion timing cannot affect ranking.
    let fedimint_reads = futures::future::join_all(sources.iter().filter_map(|source| {
        let observation = batch
            .fedimint_observations
            .iter()
            .find(|observation| observation.snapshot.id == source.id)?;
        let invoice = &invoice;
        Some(async move {
            let read = async {
                let quote = fedimint
                    .quote(observation, invoice.as_str(), amount, unix_now())
                    .await;
                let estimate = if quote.is_err() {
                    Some(
                        fedimint
                            .gateway_estimate(observation, invoice.as_str(), amount, unix_now())
                            .await,
                    )
                } else {
                    None
                };
                (quote, estimate)
            };
            // One bridge request's deadline plus margin for the estimate.
            let deadline = fedimint.request_timeout() + std::time::Duration::from_secs(1);
            let result = tokio::time::timeout(deadline, read)
                .await
                .unwrap_or_else(|_| {
                    (
                        Err("Fedimint source evaluation timed out".into()),
                        Some(Err("Gateway estimate deadline reached".into())),
                    )
                });
            (source.id.clone(), result)
        })
    }));
    let cashu_reads = futures::future::join_all(sources.iter().filter_map(|source| {
        let observation = by_id.get(&source.id).copied()?;
        if !source.capabilities.can_send || !source.capabilities.supports_lightning {
            return None;
        }
        let invoice = &invoice;
        Some(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                service.melt_quote(source.id.clone(), &observation.mint_url, invoice, amount),
            )
            .await
            .unwrap_or_else(|_| Err("Cashu source quote timed out".into()));
            (source.id.clone(), result)
        })
    }));
    // Regtest-lab only: amount-specific probes and channel state from each
    // source's own node, bound to the invoice's payee.
    let payee = payee_pubkey(invoice.as_str());
    let probe_reads = futures::future::join_all(
        sources
            .iter()
            .filter(|source| probes.is_some_and(|probes| probes.covers(source.id.as_str())))
            .filter_map(|source| {
                let probes = probes?;
                let invoice = &invoice;
                let payee = payee.as_deref();
                Some(async move {
                    let evidence = probes
                        .read(
                            source.id.as_str(),
                            invoice.as_str(),
                            payee,
                            amount,
                            unix_now(),
                        )
                        .await;
                    (source.id.clone(), evidence)
                })
            }),
    );
    // Real payment outcomes only; unknown until the lab executor records one.
    let reliability = history.map(|history| {
        let ids = sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        history.reliability(&ids, unix_now())
    });
    let (fedimint_reads, cashu_reads, probe_reads) =
        tokio::join!(fedimint_reads, cashu_reads, probe_reads);
    let mut probe_reads = probe_reads
        .into_iter()
        .filter_map(|(id, evidence)| evidence.map(|evidence| (id, evidence)))
        .collect::<BTreeMap<_, _>>();
    let mut fedimint_reads = fedimint_reads.into_iter().collect::<BTreeMap<_, _>>();
    let mut cashu_reads = cashu_reads.into_iter().collect::<BTreeMap<_, _>>();
    for source in sources {
        if source.connector_type == ConnectorType::Fedimint {
            let Some(observation) = batch
                .fedimint_observations
                .iter()
                .find(|observation| observation.snapshot.id == source.id)
            else {
                no_route_details.push(format!("{}: Fedimint observation is missing", source.id));
                continue;
            };
            let (quote_result, estimate_result) = fedimint_reads
                .remove(&source.id)
                .expect("selected federation was probed");
            let quote = match quote_result {
                Ok(quote) => quote,
                Err(error) => {
                    quote_observations.push(fedimint_quote_json(observation, None, Some(&error)));
                    match estimate_result.expect("failed quotes have estimate diagnostics") {
                        Ok(estimate) => {
                            for candidate in &estimate.candidates {
                                gateway_estimated_sources.push(json!({
                                    "source_id": source.id.as_str(),
                                    "source_label": observation.config.label,
                                    "protocol": "fedimint",
                                    "settlement_mechanism": "fedimint_lightning",
                                    "route_classification": "gateway_estimated",
                                    "executable": false,
                                    "gateway_id": candidate.gateway_id,
                                    "gateway_url": candidate.gateway_url,
                                    "gateway_protocol": candidate.gateway_protocol,
                                    "lightning_alias": candidate.lightning_alias,
                                    "gateway_fee_sats": candidate.gateway_fee_sats.sats(),
                                    "fee_base_msat": candidate.fee_base_msat,
                                    "fee_ppm": candidate.fee_ppm,
                                    "expiration_delta": candidate.expiration_delta,
                                    "federation_fee_sats": null,
                                    "funding_feasible": null,
                                    "gateway_identity_verified": true,
                                    "observed_at_unix_seconds": estimate.observed_at.unix_seconds(),
                                    "expires_at_unix_seconds": estimate.expires_at_unix_seconds,
                                    "reason": "Gateway fee is authenticated, but this federation did not provide a native read-only fee quote; federation fee, funding and execution remain unknown"
                                }));
                            }
                            quote_observations
                                .push(fedimint_gateway_estimate_json(observation, &estimate));
                        }
                        Err(estimate_error) => {
                            quote_observations.push(json!({
                                "kind": "fedimint_gateway_fee_estimate",
                                "connector": source.id.as_str(),
                                "state": "unknown",
                                "issue": {"code": "GATEWAY_ESTIMATE_UNAVAILABLE", "message": estimate_error},
                            }));
                            no_route_details.push(format!(
                                "{}: missing read-only Fedimint quote: {error}; gateway estimate unavailable: {estimate_error}",
                                source.id
                            ));
                        }
                    }
                    continue;
                }
            };
            quote_observations.push(fedimint_quote_json(observation, Some(&quote), None));
            let mut source = source;
            let total = quote.total_fee();
            source.fee = Evidence::reported(
                ecashmesh_core::FeeQuote::new(total),
                EvidenceSource::Connector,
                quote.observed_at,
                ConfidenceLevel::Medium,
            );
            if let Some(balance) = quote.spendable_balance_sats.value().copied() {
                source.liquidity = Evidence::reported(
                    ecashmesh_core::LiquidityInfo::new(balance, None),
                    EvidenceSource::Connector,
                    quote.observed_at,
                    ConfidenceLevel::Medium,
                );
            }
            // Funding is already gated by the quote; a probe measures the
            // gateway's Lightning leg, which wallet balance cannot show.
            let evidence = probe_reads.remove(&source.id);
            let applied = apply_liquidity(
                &mut source,
                evidence,
                quote.selected_gateway_id.as_deref(),
                payee.as_deref(),
                amount,
                &mut quote_observations,
            );
            if let Some(reason) = applied.exclusion {
                no_route_details.push(format!("{}: {reason}", source.id));
                continue;
            }
            // The selected gateway's budget, from the same native routing
            // info its send state machine reads when it pays.
            if let Some(route_fee) = applied.route_fee {
                let check = fee_budget::check(
                    source.id.as_str(),
                    "gateway_routing_fee_budget",
                    route_fee,
                    quote.routing_fee_budget_msat,
                );
                quote_observations.push(fee_budget_json(&check));
                fee_checks.insert(source.id.clone(), check);
            }
            if is_unavailable(&observation.health) {
                unavailable.insert(source.id.clone());
            }
            if let Some(expiry) = quote.expires_at_unix_seconds {
                expiries.push(expiry);
            }
            live_fee_terms.push(LiveFeeTerms::Fedimint {
                source_connector: source.id.clone(),
                total_fee_sats: total,
                federation_fee_sats: quote.federation_fee_sats,
                gateway_fee_sats: quote.gateway_fee_sats,
                destination_fee_sats: quote.destination_fee_sats,
            });
            live_sources.push(source);
            continue;
        }
        let Some(observation) = by_id.get(&source.id).copied() else {
            no_route_details.push(format!(
                "{}: source metadata observation is missing",
                source.id
            ));
            continue;
        };
        if !source.capabilities.can_send || !source.capabilities.supports_lightning {
            no_route_details.push(format!(
                "{}: {}",
                source.id,
                observation.send_unavailable_reason(amount).unwrap_or(
                    "incompatible capabilities; no fresh Cashu bolt11/sat send mechanism"
                )
            ));
            continue;
        }
        let quote = match cashu_reads
            .remove(&source.id)
            .expect("eligible Cashu source was probed")
        {
            Ok(quote) => quote,
            Err(error) => {
                no_route_details.push(format!("{}: source melt quote failed: {error}", source.id));
                quote_observations.push(json!({
                    "kind": "source_melt_quote", "connector": source.id.as_str(), "state": "unknown",
                    "issue": {"code": "QUOTE_UNAVAILABLE", "message": error},
                }));
                continue;
            }
        };
        quote_observations.push(melt_quote_json(observation, &quote));
        let Some(quote_evidence) = quote.evidence.observation() else {
            let detail = quote.issue.map_or_else(
                || "source mint did not return a usable melt quote".into(),
                |issue| format!("{}: {}", issue.code, issue.message),
            );
            no_route_details.push(format!("{}: missing quote: {detail}", source.id));
            continue;
        };
        // The wallet funds a melt with the amount plus the fee reserve; a
        // balance it reported that falls short rules the mint out.
        if let Some(&balance) =
            ecashmesh_cashu::discovery::canonical_mint_url(&observation.mint_url)
                .ok()
                .and_then(|url| balances.get(&url))
        {
            let required = amount
                .sats()
                .saturating_add(quote_evidence.value.fee_reserve_sats.sats());
            quote_observations.push(json!({
                "kind": "wallet_funding", "connector": source.id.as_str(), "state": "known",
                "value": {"balance_sats": balance, "required_sats": required,
                          "funding_feasible": balance >= required, "source": "wallet_reported"},
            }));
            if balance < required {
                no_route_details.push(format!(
                    "{}: the wallet holds {balance} sats at this mint; paying needs {required} (amount plus fee reserve)",
                    source.id
                ));
                continue;
            }
        }
        let mut source = source;
        let evidence = probe_reads.remove(&source.id);
        let applied = apply_liquidity(
            &mut source,
            evidence,
            None,
            payee.as_deref(),
            amount,
            &mut quote_observations,
        );
        if let Some(reason) = applied.exclusion {
            no_route_details.push(format!("{}: {reason}", source.id));
            continue;
        }
        // CDK pays a melt with the quote's fee reserve as the fee limit.
        if let Some(route_fee) = applied.route_fee {
            let check = fee_budget::check(
                source.id.as_str(),
                "melt_fee_reserve",
                route_fee,
                quote_evidence
                    .value
                    .fee_reserve_sats
                    .sats()
                    .checked_mul(1_000),
            );
            quote_observations.push(fee_budget_json(&check));
            fee_checks.insert(source.id.clone(), check);
        }
        source.fee = Evidence::reported(
            ecashmesh_core::FeeQuote::new(quote_evidence.value.fee_reserve_sats),
            quote_evidence.source,
            quote_evidence.observed_at,
            quote_evidence.confidence,
        );
        live_fee_terms.push(LiveFeeTerms::Cashu {
            source_connector: source.id.clone(),
            fee_reserve_sats: quote_evidence.value.fee_reserve_sats,
            input_fees: observation.input_fees.clone(),
        });
        if let Some(expiry) = quote_evidence.value.expires_at_unix_seconds {
            expiries.push(expiry);
        }
        if is_unavailable(&source.evidence.health) {
            unavailable.insert(source.id.clone());
        }
        live_sources.push(source);
    }
    // Evidence for sources that failed earlier remains inspectable.
    for liquidity in probe_reads.values() {
        quote_observations.extend(
            liquidity
                .probe
                .iter()
                .map(|evidence| probe_json(evidence, "not_applied", None)),
        );
        quote_observations.extend(
            liquidity
                .channels
                .iter()
                .map(|state| channel_json(state, "not_applied", None)),
        );
    }
    if let Some(reliability) = &reliability {
        for source in &mut live_sources {
            if let Some(stats) = reliability.get(source.id.as_str()) {
                apply_reliability(source, stats);
            }
        }
        quote_observations.extend(reliability.values().map(|stats| {
            json!({
                "kind": "payment_reliability",
                "connector": stats.source_id,
                "state": stats.freshness,
                "value": stats,
            })
        }));
    }
    if let Some(history) = history {
        history
            .record(&history_records(&quote_observations, amount))
            .await;
    }
    let ranked = match rank_feasible_sources(
        live_sources,
        &unavailable,
        &fee_checks,
        no_route_details,
        amount,
    ) {
        Ok(ranked) => ranked,
        Err(details) => {
            return Err(LiveNoRoute {
                details,
                quote_observations,
            });
        }
    };
    let live_sources = ranked.sources;
    let expires_at_unix_seconds = expiries
        .into_iter()
        .chain(std::iter::once(batch.expires_at_unix_seconds))
        .min()
        .unwrap_or_else(unix_now);
    let source_context = live_sources
        .iter()
        .map(|source| {
            json!({
                "connector": source.id.as_str(),
                "role": "wallet_source",
            })
        })
        .collect::<Vec<_>>();
    let mut mechanisms = live_sources
        .iter()
        .map(|source| match source.connector_type {
            ConnectorType::Fedimint => "fedimint_lightning",
            _ => "cashu_lightning",
        })
        .collect::<Vec<_>>();
    mechanisms.sort_unstable();
    mechanisms.dedup();
    let route_shape = match (destination_type, mechanisms.as_slice()) {
        ("cashu", ["cashu_lightning"]) => {
            "Cashu source → Lightning invoice → Cashu destination quote"
        }
        ("cashu", _) => {
            "Configured source → destination Lightning invoice → Cashu destination quote"
        }
        (_, ["cashu_lightning"]) => "Cashu source → Lightning invoice",
        _ => "Configured Cashu/Fedimint source → Lightning invoice",
    };
    Ok(LiveEvaluation {
        ranking: ranked.ranking,
        connectors: live_sources,
        quote_observations,
        fee_terms: live_fee_terms,
        expires_at_unix_seconds,
        graph_context: json!({
            "destination": destination_context,
            "sources": source_context,
            "mechanisms": mechanisms,
            "route_shape": route_shape,
            "route_classification": "quote_backed",
            "execution": "No wallet proofs, funds, or payment instruction were supplied to EcashMesh",
        }),
        gateway_estimated_sources: {
            gateway_estimated_sources.sort_by(|left, right| {
                left["gateway_fee_sats"]
                    .as_u64()
                    .cmp(&right["gateway_fee_sats"].as_u64())
                    .then_with(|| left["source_id"].as_str().cmp(&right["source_id"].as_str()))
            });
            gateway_estimated_sources
        },
    })
}

/// A destination reduced to the Lightning invoice every source pays.
pub(crate) struct ResolvedDestination {
    pub invoice: LightningInvoice,
    pub context: Value,
    pub expiry: Option<u64>,
}

/// Turns the destination into one Lightning invoice: a BOLT11 invoice as is,
/// or a Cashu request's embedded invoice or a fresh mint quote from one of
/// its mints. Quote observations are appended to `observations`.
#[allow(clippy::too_many_lines)] // One branch per destination kind, with exact no-route reasons.
pub(crate) async fn resolve_destination(
    service: &DiscoveryService,
    batch: &ConnectorBatch,
    destination: LiveDestination,
    amount: Amount,
    observations: &mut Vec<Value>,
) -> Result<ResolvedDestination, Vec<String>> {
    let (invoice, context, expiry) = match destination {
        LiveDestination::Lightning(invoice) => {
            if invoice.amount() != Some(amount) {
                return Err(vec![
                        "unsupported destination: Lightning invoice amount must exactly match the requested amount"
                            .into(),
                    ]);
            }
            (
                invoice,
                json!({
                    "type": "lightning",
                    "normalization": "checksummed_bolt11",
                    "invoice_amount_sats": amount.sats(),
                }),
                None,
            )
        }
        LiveDestination::Cashu(request) => {
            if !request.supported_methods.is_empty()
                && !request
                    .supported_methods
                    .iter()
                    .any(|method| method.method == "bolt11")
            {
                return Err(vec![
                    "route constraints: NUT-18 request does not permit a bolt11 payment leg".into(),
                ]);
            }
            if request.amount.is_some_and(|claimed| claimed != amount) {
                return Err(vec![
                        "route constraints: Cashu payment request amount does not match requested amount"
                            .into(),
                    ]);
            }
            let targets = request
                .mint_urls
                .iter()
                .filter_map(|mint_url| {
                    batch
                        .live_observations
                        .iter()
                        .find(|observation| &observation.mint_url == mint_url)
                })
                .collect::<Vec<_>>();
            if targets.is_empty() {
                return Err(vec![
                    "destination mint was not discovered or has no inspectable observation".into(),
                ]);
            }
            let embedded_invoice = request.invoice.clone();
            let (target, invoice, expiry) = if let Some(invoice) = embedded_invoice {
                let target = targets[0];
                observations.push(json!({
                    "kind": "destination_invoice",
                    "state": "known",
                    "origin": "cashu_payment_request",
                    "mint_url": target.mint_url,
                }));
                if invoice.amount() != Some(amount) {
                    return Err(vec![
                            "route constraints: Cashu payment request invoice amount does not match requested amount"
                                .into(),
                        ]);
                }
                (target, invoice, None)
            } else {
                let mut failures = Vec::new();
                let mut usable = None;
                for target in targets {
                    match service
                        .mint_quote(target.id.clone(), &target.mint_url, amount)
                        .await
                    {
                        Ok(quote) => {
                            observations.push(mint_quote_json(target, &quote));
                            if let Some(observed) = quote.evidence.observation() {
                                usable = Some((
                                    target,
                                    observed.value.invoice.clone(),
                                    observed.value.expires_at_unix_seconds,
                                ));
                                break;
                            }
                            let detail = quote.issue.map_or_else(
                                || "destination mint did not return a usable mint quote".into(),
                                |issue| format!("{}: {}", issue.code, issue.message),
                            );
                            failures.push(format!("{}: {detail}", target.mint_url));
                        }
                        Err(error) => failures.push(format!("{}: {error}", target.mint_url)),
                    }
                }
                let Some(usable) = usable else {
                    return Err(failures
                        .into_iter()
                        .map(|failure| format!("missing quote: {failure}"))
                        .collect());
                };
                usable
            };
            (
                invoice,
                json!({
                    "type": "cashu",
                    "mint_url": target.mint_url,
                    "mint_urls": request.mint_urls,
                    "mints_are_preferred": request.mints_are_preferred,
                    "unit": request.unit,
                    "transports": request.transports.iter().map(|transport| json!({
                        "type": transport.kind,
                        "target": transport.target,
                    })).collect::<Vec<_>>(),
                    "supported_methods": request.supported_methods.iter().map(|method| json!({
                        "method": method.method,
                        "receiver_fee_sats": method.fee.map(Amount::sats),
                    })).collect::<Vec<_>>(),
                    "normalization": request.encoding.code(),
                    "transfer": "cashu_mint_quote_to_lightning_invoice",
                }),
                expiry,
            )
        }
    };
    Ok(ResolvedDestination {
        invoice,
        context,
        expiry,
    })
}

/// A ranking over the sources that passed feasibility.
struct RankedSources {
    ranking: RouteRanking,
    sources: Vec<ConnectorSnapshot>,
}

/// Feasibility, then ranking. A source known to be unavailable, or whose
/// measured route fee exceeds its routing-fee budget, is dropped here, before the ranker sees it, so it can
/// neither be recommended nor outrank an executable source. With no source
/// left this is the usual no-route outcome; it never falls back to an
/// infeasible source.
fn rank_feasible_sources(
    live_sources: Vec<ConnectorSnapshot>,
    unavailable: &BTreeSet<ConnectorId>,
    fee_checks: &BTreeMap<ConnectorId, FeeBudgetCheck>,
    mut no_route_details: Vec<String>,
    amount: Amount,
) -> Result<RankedSources, Vec<String>> {
    let mut feasible = Vec::with_capacity(live_sources.len());
    for source in live_sources {
        if unavailable.contains(&source.id) {
            no_route_details.push(format!("{}: source is unavailable", source.id));
        } else if let Some(check) = fee_checks.get(&source.id).filter(|c| c.infeasible()) {
            no_route_details.push(format!("{}: {}", source.id, check.message()));
        } else {
            feasible.push(source);
        }
    }
    let live_sources = feasible;
    if live_sources.is_empty() {
        if no_route_details.is_empty() {
            no_route_details.push("No source connectors can quote an executable route".into());
        }
        return Err(no_route_details);
    }
    let ranking = rank_sources(&live_sources, amount);
    if ranking.ranked.is_empty() {
        return Err(no_route_details
            .into_iter()
            .chain(std::iter::once(
                "All quote-backed candidates were rejected during feasibility evaluation".into(),
            ))
            .collect());
    }
    fee_budget_invariant(&ranking, fee_checks).map_err(|violation| vec![violation])?;
    Ok(RankedSources {
        ranking,
        sources: live_sources,
    })
}

/// One native-settlement candidate per source, ranked by the core engine.
fn rank_sources(live_sources: &[ConnectorSnapshot], amount: Amount) -> RouteRanking {
    let evidence = live_sources
        .iter()
        .map(|source| (source.id.clone(), source.evidence.clone()))
        .collect();
    rank_routes(
        PaymentRequest::new(amount),
        live_sources
            .iter()
            .filter_map(|source| source.quote(amount).ok()),
        &evidence,
        EvidenceTimestamp::from_unix_seconds(unix_now()),
        RouteRankingConfig::default(),
    )
}

/// Evaluator-vs-execution invariant: no ranked route, least of all the
/// selected one, uses a source whose route fee is known to exceed the budget
/// its node pays from. A violation fails the evaluation instead of
/// recommending a payment the gateway or mint would refuse.
fn fee_budget_invariant(
    ranking: &RouteRanking,
    fee_checks: &BTreeMap<ConnectorId, FeeBudgetCheck>,
) -> Result<(), String> {
    for route in &ranking.ranked {
        for hop in &route.candidate.hops {
            if let Some(check) = fee_checks
                .get(&hop.connector_id)
                .filter(|check| check.infeasible())
            {
                return Err(format!(
                    "evaluator invariant violated: {} was ranked although {}",
                    hop.connector_id,
                    check.message()
                ));
            }
        }
    }
    Ok(())
}

/// What lab liquidity evidence decided for one source.
#[derive(Default)]
struct AppliedLiquidity {
    /// The source cannot route the amount.
    exclusion: Option<String>,
    /// The routing fee its node would pay; `None` without lab evidence.
    route_fee: Option<RouteFee>,
}

/// Applies regtest-lab liquidity evidence to one quote-backed source and
/// records it. Returns an exclusion reason when the source cannot route the
/// amount, and the route fee measured from the same evidence. Fedimint
/// evidence must come from the gateway the quote selected.
fn apply_liquidity(
    source: &mut ConnectorSnapshot,
    evidence: Option<SourceLiquidity>,
    selected_gateway: Option<&str>,
    payee: Option<&str>,
    amount: Amount,
    observations: &mut Vec<Value>,
) -> AppliedLiquidity {
    let Some(SourceLiquidity { probe, channels }) = evidence else {
        return AppliedLiquidity::default();
    };
    let not_selected = |node: Option<&str>| matches!((node, selected_gateway), (Some(node), Some(gateway)) if node != gateway);
    let issue = || {
        Some((
            "PROBE_NODE_NOT_SELECTED_GATEWAY",
            "evidence is from a node other than the quoted gateway".to_owned(),
        ))
    };
    let probe = match probe {
        Some(probe) if not_selected(probe.node_pubkey.as_deref()) => {
            observations.push(probe_json(&probe, "not_applied", issue()));
            None
        }
        probe => probe,
    };
    let channels = match channels {
        Some(state) if not_selected(state.node_pubkey.as_deref()) => {
            observations.push(channel_json(&state, "not_applied", issue()));
            None
        }
        state => state,
    };
    let now = unix_now();
    let (effect, basis) =
        crate::liquidity::combined_effect(probe.as_ref(), channels.as_ref(), amount, now);
    let (label, issue, exclusion) = match effect {
        ProbeEffect::Liquidity(liquidity) => {
            source.liquidity = liquidity;
            ("liquidity", None, None)
        }
        ProbeEffect::Exclude(reason) => (
            "excluded",
            Some(("LIGHTNING_LIQUIDITY_UNAVAILABLE", reason.clone())),
            Some(reason),
        ),
        ProbeEffect::Keep => ("none", None, None),
    };
    let decided = |which: LiquidityBasis| if basis == which { label } else { "supporting" };
    if let Some(probe) = &probe {
        observations.push(probe_json(
            probe,
            decided(LiquidityBasis::ActiveProbe),
            (basis == LiquidityBasis::ActiveProbe)
                .then(|| issue.clone())
                .flatten(),
        ));
    }
    if let Some(state) = &channels {
        observations.push(channel_json(
            state,
            decided(LiquidityBasis::ChannelState),
            (basis == LiquidityBasis::ChannelState)
                .then(|| issue.clone())
                .flatten(),
        ));
    }
    observations.push(json!({
        "kind": "lightning_liquidity",
        "connector": source.id.as_str(),
        "state": match source.liquidity.freshness() {
            ecashmesh_core::EvidenceFreshness::Fresh => "fresh",
            ecashmesh_core::EvidenceFreshness::Stale => "stale",
            ecashmesh_core::EvidenceFreshness::Unknown => "unknown",
        },
        "effect": label,
        "value": {
            "basis": basis,
            "applied_to_ranking": label == "liquidity",
            "confidence": source.liquidity.observation().map(|observation| confidence_code(observation.confidence)),
            "observed_at_unix_seconds": source.liquidity.observation().map(|observation| observation.observed_at.unix_seconds()),
            "amount_sats": amount.sats(),
        },
        "issue": issue.map(|(code, message)| json!({"code": code, "message": message})),
    }));
    AppliedLiquidity {
        exclusion,
        route_fee: Some(fee_budget::route_fee(
            probe.as_ref(),
            channels.as_ref(),
            payee,
            selected_gateway,
            amount,
            now,
        )),
    }
}

fn fee_budget_json(check: &FeeBudgetCheck) -> Value {
    json!({
        "kind": "routing_fee_budget",
        "connector": check.source_id,
        "state": if check.feasible.is_some() { "fresh" } else { "unknown" },
        "effect": match check.feasible {
            Some(true) => "feasible",
            Some(false) => "excluded",
            None => "none",
        },
        "value": check,
        "issue": check.infeasible().then(|| json!({
            "code": fee_budget::INFEASIBLE_FEE_BUDGET,
            "message": check.message(),
        })),
    })
}

/// Real payment outcomes become both the hop's and the connector's
/// reliability (the model keeps the two levels separate; in this single-hop
/// graph both describe payments through this source). The earliest recorded
/// observation is the connector's first-observed time.
fn apply_reliability(source: &mut ConnectorSnapshot, stats: &crate::reliability::ReliabilityStats) {
    let evidence = crate::reliability::evidence(stats);
    source.reliability = evidence.clone();
    source.evidence.reliability = evidence;
    if let Some(first) = stats.first_observed_at_unix_seconds {
        source.evidence.first_observed_at = Some(EvidenceTimestamp::from_unix_seconds(first));
    }
}

/// One persisted observation per source: liquidity and gateway state as seen
/// for this amount. Never a payment outcome.
fn history_records(observations: &[Value], amount: Amount) -> Vec<Value> {
    let now = unix_now();
    observations
        .iter()
        .filter(|observation| observation["kind"] == "lightning_liquidity")
        .map(|summary| {
            let source = summary["connector"].clone();
            let find = |kind: &str| {
                observations
                    .iter()
                    .find(|observation| observation["kind"] == kind && observation["connector"] == source)
                    .map(|observation| &observation["value"])
            };
            let channels = find("lightning_channel_state");
            json!({
                "kind": "liquidity_observation",
                "source_id": source,
                "timestamp": now,
                "amount_sats": amount.sats(),
                "basis": summary["value"]["basis"],
                "effect": summary["effect"],
                "probe_outcome": find("lightning_liquidity_probe").map(|probe| probe["outcome"].clone()),
                "channel_outbound_sats": channels.map(|state| state["outbound_sats"].clone()),
                "active_channel_count": channels.map(|state| state["active_channel_count"].clone()),
                "node_state": channels.map(|state| state["node_state"].clone()),
            })
        })
        .collect()
}

fn probe_json(evidence: &LiquidityEvidence, effect: &str, issue: Option<(&str, String)>) -> Value {
    json!({
        "kind": "lightning_liquidity_probe",
        "connector": evidence.source_id,
        "state": evidence.freshness(unix_now()),
        "effect": effect,
        "value": evidence,
        "issue": issue.map(|(code, message)| json!({"code": code, "message": message})),
    })
}

fn channel_json(state: &ChannelState, effect: &str, issue: Option<(&str, String)>) -> Value {
    json!({
        "kind": "lightning_channel_state",
        "connector": state.source_id,
        "state": state.freshness(unix_now()),
        "effect": effect,
        "value": state,
        "issue": issue.map(|(code, message)| json!({"code": code, "message": message})),
    })
}

const fn confidence_code(confidence: ConfidenceLevel) -> &'static str {
    match confidence {
        ConfidenceLevel::None => "none",
        ConfidenceLevel::Low => "low",
        ConfidenceLevel::Medium => "medium",
        ConfidenceLevel::High => "high",
    }
}

/// The invoice's payee node, used to recognise a direct channel. `None` when
/// the invoice cannot be decoded (channel state then proves nothing).
fn payee_pubkey(invoice: &str) -> Option<String> {
    invoice
        .parse::<lightning_invoice::Bolt11Invoice>()
        .ok()
        .map(|invoice| invoice.get_payee_pub_key().to_string())
}

/// Only a fresh observation of an outage excludes a source; unknown or stale
/// health is ranked with its penalty instead.
fn is_unavailable(health: &Evidence<ConnectorHealth>) -> bool {
    matches!(health, Evidence::Known(observation) if observation.value == ConnectorHealth::Unavailable)
}

fn mint_quote_json(target: &CashuObservation, quote: &QuoteObservation<MintQuote>) -> Value {
    quote_json(
        "destination_mint_quote",
        &target.id,
        &target.mint_url,
        quote,
        |value| {
            json!({
                "quote_id": value.quote_id,
                "expiry": value.expires_at_unix_seconds,
                // The invoice a source pays; the wallet claims the minted
                // ecash with `quote_id` once it is paid.
                "invoice": value.invoice.as_str(),
                "invoice_amount_sats": value.invoice.amount().map(Amount::sats),
            })
        },
    )
}

fn melt_quote_json(
    source: &CashuObservation,
    quote: &QuoteObservation<ecashmesh_cashu::MeltQuote>,
) -> Value {
    quote_json(
        "source_melt_quote",
        &source.id,
        &source.mint_url,
        quote,
        |value| {
            json!({
                "quote_id": value.quote_id,
                "fee_reserve_sats": value.fee_reserve_sats.sats(),
                "fee_kind": "reserve_upper_bound",
                "expiry": value.expires_at_unix_seconds,
            })
        },
    )
}

fn fedimint_quote_json(
    source: &FederationObservation,
    quote: Option<&FedimintQuote>,
    issue: Option<&str>,
) -> Value {
    json!({
        "kind": "fedimint_lightning_fee_quote",
        "connector": source.snapshot.id.as_str(),
        "federation_id": source.config.federation_id,
        "state": if quote.is_some() { "known" } else { "unknown" },
        "observed_at_unix_seconds": quote.map(|quote| quote.observed_at.unix_seconds()),
        "value": quote.map(|quote| json!({
            "federation_fee_sats": quote.federation_fee_sats.sats(),
            "gateway_fee_sats": quote.gateway_fee_sats.map(Amount::sats),
            "destination_fee_sats": quote.destination_fee_sats.map(Amount::sats),
            "fee_kind": "estimate",
            "total_fee_sats": quote.total_fee().sats(),
            "native_evidence": quote.native_evidence,
            "gateway_id": quote.selected_gateway_id,
            "payable": quote.payable,
            "spendable_balance_sats": quote.spendable_balance_sats.value().map(|amount| amount.sats()),
            "metrics": quote.metrics,
            "expiry": quote.expires_at_unix_seconds,
        })),
        "issue": issue.map(|message| json!({"code": "READ_ONLY_QUOTE_UNAVAILABLE", "message": message})),
    })
}

fn fedimint_gateway_estimate_json(
    source: &FederationObservation,
    estimate: &ecashmesh_fedimint::FedimintGatewayEstimate,
) -> Value {
    json!({
        "kind": "fedimint_gateway_fee_estimate",
        "connector": source.snapshot.id.as_str(),
        "federation_id": source.config.federation_id,
        "state": "known",
        "observed_at_unix_seconds": estimate.observed_at.unix_seconds(),
        "value": {
            "gateway_fee_sats": estimate.gateway_fee_sats.sats(),
            "selected_gateway_id": estimate.selected_gateway_id,
            "gateway_protocol": estimate.gateway_protocol,
            "gateway_candidates": estimate.candidates.iter().map(|candidate| json!({
                "gateway_id": candidate.gateway_id,
                "gateway_url": candidate.gateway_url,
                "gateway_fee_sats": candidate.gateway_fee_sats.sats(),
                "fee_base_msat": candidate.fee_base_msat,
                "fee_ppm": candidate.fee_ppm,
                "expiration_delta": candidate.expiration_delta,
                "gateway_protocol": candidate.gateway_protocol,
                "lightning_alias": candidate.lightning_alias,
            })).collect::<Vec<_>>(),
            "gateway_identity_verified": true,
            "federation_fee_sats": null,
            "funding_feasible": null,
            "payable": null,
            "expiry": estimate.expires_at_unix_seconds,
        },
    })
}

fn quote_json<T>(
    kind: &str,
    connector: &ConnectorId,
    mint_url: &str,
    quote: &QuoteObservation<T>,
    value: impl FnOnce(&T) -> Value,
) -> Value {
    let evidence = &quote.evidence;
    json!({
        "kind": kind,
        "connector": connector.as_str(),
        "mint_url": mint_url,
        "endpoint": quote.endpoint,
        "state": match evidence { Evidence::Known(_) => "known", Evidence::Stale(_) => "stale", Evidence::Unknown => "unknown" },
        "observed_at_unix_seconds": evidence.observed_at().map(EvidenceTimestamp::unix_seconds),
        "value": evidence.value().map(value),
        "issue": &quote.issue,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fee_budget::RouteFeeBasis;
    use ecashmesh_core::{ConnectorCapabilities, ConnectorEvidence, FeeQuote, LiquidityInfo};

    fn source(id: &str, connector_type: ConnectorType, fee_sats: u64) -> ConnectorSnapshot {
        let id = ConnectorId::new(id).unwrap();
        let now = EvidenceTimestamp::from_unix_seconds(unix_now());
        let reported = |confidence| (EvidenceSource::Connector, now, confidence);
        let (origin, at, confidence) = reported(ConfidenceLevel::Medium);
        ConnectorSnapshot {
            id: id.clone(),
            connector_type,
            capabilities: ConnectorCapabilities::new(true, false, false, true),
            liquidity: Evidence::reported(
                LiquidityInfo::new(Amount::from_sats(100_000), None),
                origin,
                at,
                confidence,
            ),
            fee: Evidence::reported(
                FeeQuote::new(Amount::from_sats(fee_sats)),
                origin,
                at,
                confidence,
            ),
            reliability: Evidence::Unknown,
            evidence: ConnectorEvidence::new(
                id,
                None,
                Evidence::Unknown,
                Evidence::Unknown,
                Evidence::Unknown,
            ),
        }
    }

    fn fedimint(id: &str, fee_sats: u64) -> ConnectorSnapshot {
        source(id, ConnectorType::Fedimint, fee_sats)
    }

    fn checks(
        verdicts: &[(&str, &'static str, u64, u64)],
    ) -> BTreeMap<ConnectorId, FeeBudgetCheck> {
        verdicts
            .iter()
            .map(|&(id, kind, probe_fee, budget)| {
                (
                    ConnectorId::new(id).unwrap(),
                    fee_budget::check(
                        id,
                        kind,
                        RouteFee::Exact {
                            msat: probe_fee,
                            basis: RouteFeeBasis::LightningProbe,
                        },
                        Some(budget),
                    ),
                )
            })
            .collect()
    }

    fn rank(
        sources: Vec<ConnectorSnapshot>,
        fee_checks: &BTreeMap<ConnectorId, FeeBudgetCheck>,
    ) -> Result<RankedSources, Vec<String>> {
        rank_feasible_sources(
            sources,
            &BTreeSet::new(),
            fee_checks,
            Vec::new(),
            Amount::from_sats(1_000),
        )
    }

    fn order(ranked: &RankedSources) -> Vec<String> {
        ranked
            .ranking
            .ranked
            .iter()
            .map(|route| route.candidate.hops[0].connector_id.to_string())
            .collect()
    }

    /// The selected Fedimint source's measured route fee fits the budget of
    /// the gateway that will pay it.
    fn assert_selected_is_executable(
        ranked: &RankedSources,
        fee_checks: &BTreeMap<ConnectorId, FeeBudgetCheck>,
    ) {
        let selected = &ranked.ranking.ranked[0].candidate.hops[0].connector_id;
        let check = &fee_checks[selected];
        assert!(
            check.probe_fee_msat.unwrap() <= check.fee_budget_msat.unwrap(),
            "selected {selected} cannot pay its route"
        );
    }

    #[test]
    fn an_unaffordable_cheap_gateway_is_excluded_before_ranking() {
        // Fed A quotes the lowest fee, so it outranks Fed B on evidence alone.
        let sources = || vec![fedimint("fedimint:a", 1), fedimint("fedimint:b", 9)];
        let unchecked = rank(sources(), &BTreeMap::new()).unwrap();
        assert_eq!(order(&unchecked), ["fedimint:a", "fedimint:b"]);

        // A's gateway has no routing budget for its 1,000 msat route.
        let fee_checks = checks(&[
            ("fedimint:a", "gateway_routing_fee_budget", 1_000, 0),
            ("fedimint:b", "gateway_routing_fee_budget", 5_000, 6_000),
        ]);
        let ranked = rank(sources(), &fee_checks).unwrap();
        assert_eq!(order(&ranked), ["fedimint:b"]);
        assert_eq!(ranked.sources.len(), 1);
        assert_selected_is_executable(&ranked, &fee_checks);
    }

    #[test]
    fn a_lower_route_fee_does_not_outrank_an_executable_route() {
        // B's route is cheaper than C's, but only C's gateway can pay its own.
        let fee_checks = checks(&[
            ("fedimint:a", "gateway_routing_fee_budget", 0, 0),
            ("fedimint:b", "gateway_routing_fee_budget", 5_000, 4_000),
            ("fedimint:c", "gateway_routing_fee_budget", 7_000, 10_000),
        ]);
        let sources = vec![
            fedimint("fedimint:a", 5),
            fedimint("fedimint:b", 1),
            fedimint("fedimint:c", 5),
        ];
        let ranked = rank(sources, &fee_checks).unwrap();
        assert!(!order(&ranked).contains(&"fedimint:b".to_owned()));
        assert_eq!(order(&ranked).len(), 2);
        assert_selected_is_executable(&ranked, &fee_checks);
    }

    #[test]
    fn no_executable_candidate_is_a_no_route_outcome() {
        let fee_checks = checks(&[
            ("fedimint:a", "gateway_routing_fee_budget", 1_000, 0),
            ("fedimint:b", "gateway_routing_fee_budget", 5_000, 4_000),
        ]);
        let Err(details) = rank(
            vec![fedimint("fedimint:a", 1), fedimint("fedimint:b", 2)],
            &fee_checks,
        ) else {
            panic!("an infeasible source was selected");
        };
        assert_eq!(details.len(), 2);
        assert!(details[0].starts_with("fedimint:a: Measured Lightning route fee 1000 msat"));
        assert!(details[1].contains("routing-fee budget of 4000 msat"));
    }

    #[test]
    fn undetermined_feasibility_ranks_as_before() {
        let fee_checks = BTreeMap::from([(
            ConnectorId::new("fedimint:a").unwrap(),
            fee_budget::check(
                "fedimint:a",
                "gateway_routing_fee_budget",
                RouteFee::Unknown,
                Some(0),
            ),
        )]);
        let sources = || vec![fedimint("fedimint:a", 1), fedimint("fedimint:b", 9)];
        assert_eq!(
            order(&rank(sources(), &fee_checks).unwrap()),
            order(&rank(sources(), &BTreeMap::new()).unwrap())
        );
    }

    #[test]
    fn cashu_ranking_within_the_fee_reserve_is_unchanged() {
        let sources = || {
            vec![
                source("cashu:mint-a", ConnectorType::Cashu, 3),
                source("cashu:mint-b", ConnectorType::Cashu, 2),
                fedimint("fedimint:c", 4),
            ]
        };
        let before = rank(sources(), &BTreeMap::new()).unwrap();
        // Lab mints reserve 2% (20,000 msat at 1,000 sats); routes cost less.
        let fee_checks = checks(&[
            ("cashu:mint-a", "melt_fee_reserve", 0, 20_000),
            ("cashu:mint-b", "melt_fee_reserve", 2_002, 20_000),
        ]);
        let after = rank(sources(), &fee_checks).unwrap();
        assert_eq!(order(&after), order(&before));
        let scores = |ranked: &RankedSources| {
            ranked
                .ranking
                .ranked
                .iter()
                .map(|route| route.score)
                .collect::<Vec<_>>()
        };
        assert_eq!(scores(&after), scores(&before));
    }

    #[test]
    fn ranking_an_infeasible_source_violates_the_invariant() {
        let sources = vec![fedimint("fedimint:a", 1)];
        let ranking = rank_sources(&sources, Amount::from_sats(1_000));
        let feasible = checks(&[("fedimint:a", "gateway_routing_fee_budget", 0, 0)]);
        assert_eq!(fee_budget_invariant(&ranking, &feasible), Ok(()));
        let infeasible = checks(&[("fedimint:a", "gateway_routing_fee_budget", 1, 0)]);
        let violation = fee_budget_invariant(&ranking, &infeasible).unwrap_err();
        assert!(violation.starts_with("evaluator invariant violated: fedimint:a was ranked"));
    }
}
