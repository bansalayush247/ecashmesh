//! Quote-backed live route construction.
//!
//! This module is intentionally a thin composition layer: Cashu protocol I/O
//! and normalization remain in `ecashmesh-cashu`; bounded route generation and
//! ranking remain in `ecashmesh-core`. It never creates a mint-to-mint edge.

use std::{collections::BTreeMap, sync::Arc};

use ecashmesh_cashu::{
    CashuObservation, CashuPaymentRequest, KeysetFee, MintQuote, QuoteObservation,
    discovery::DiscoveryService,
};
use ecashmesh_core::{
    Amount, AmountAwareEvidence, ConfidenceLevel, ConnectorCapabilities, ConnectorEvidence,
    ConnectorHealth, ConnectorId, ConnectorRegistry, ConnectorSnapshot, ConnectorType,
    DiscoveredConnector, Evidence, EvidenceSource, EvidenceState, EvidenceTimestamp,
    ExecutableEdge, GraphBuilder, GraphNode, GraphSnapshotPublisher, HealthObservation,
    HealthState, LightningInvoice, RouteRanking, RouteSearchConfig, RouteSearchRequest,
    ScalableRouter, SearchEndpoint, TransferMechanism,
};
use ecashmesh_fedimint::{FederationObservation, FedimintQuote, FedimintService};
use serde_json::{Value, json};

use crate::connectors::{ConnectorBatch, unix_now};
use crate::history::LabHistory;
use crate::probe::{
    ChannelState, LiquidityBasis, LiquidityEvidence, LiquidityProbes, ProbeEffect, SourceLiquidity,
};

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

/// A successful, quote-backed graph search plus data for API inspection.
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
/// and evaluates the resulting immutable graph through the Phase 9 router.
#[allow(clippy::too_many_lines)] // Keeps the full live feasibility chain and its exact no-route diagnostics together.
#[allow(clippy::too_many_arguments)] // Each optional lab evidence source is an explicit input.
pub(super) async fn evaluate(
    service: &Arc<DiscoveryService>,
    fedimint: &Arc<FedimintService>,
    probes: Option<&LiquidityProbes>,
    history: Option<&LabHistory>,
    batch: &ConnectorBatch,
    sources: Vec<ConnectorSnapshot>,
    destination: LiveDestination,
    amount: Amount,
) -> Result<LiveEvaluation, LiveNoRoute> {
    let mut quote_observations = Vec::new();
    let destination_type = destination.type_code();
    let (invoice, destination_context, destination_expiry) = match destination {
        LiveDestination::Lightning(invoice) => {
            if invoice.amount() != Some(amount) {
                return Err(LiveNoRoute {
                    details: vec![
                        "unsupported destination: Lightning invoice amount must exactly match the requested amount"
                            .into(),
                    ],
                    quote_observations,
                });
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
                return Err(LiveNoRoute {
                    details: vec![
                        "route constraints: NUT-18 request does not permit a bolt11 payment leg"
                            .into(),
                    ],
                    quote_observations,
                });
            }
            if request.amount.is_some_and(|claimed| claimed != amount) {
                return Err(LiveNoRoute {
                    details: vec![
                        "route constraints: Cashu payment request amount does not match requested amount"
                            .into(),
                    ],
                    quote_observations,
                });
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
                return Err(LiveNoRoute {
                    details: vec![
                        "destination mint was not discovered or has no inspectable observation"
                            .into(),
                    ],
                    quote_observations,
                });
            }
            let embedded_invoice = request.invoice.clone();
            let (target, invoice, expiry) = if let Some(invoice) = embedded_invoice {
                let target = targets[0];
                quote_observations.push(json!({
                    "kind": "destination_invoice",
                    "state": "known",
                    "origin": "cashu_payment_request",
                    "mint_url": target.mint_url,
                }));
                if invoice.amount() != Some(amount) {
                    return Err(LiveNoRoute {
                        details: vec![
                            "route constraints: Cashu payment request invoice amount does not match requested amount"
                                .into(),
                        ],
                        quote_observations,
                    });
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
                            quote_observations.push(mint_quote_json(target, &quote));
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
                    return Err(LiveNoRoute {
                        details: failures
                            .into_iter()
                            .map(|failure| format!("missing quote: {failure}"))
                            .collect(),
                        quote_observations,
                    });
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

    let by_id = batch
        .live_observations
        .iter()
        .map(|observation| (observation.id.clone(), observation))
        .collect::<BTreeMap<_, _>>();
    let mut live_sources = Vec::new();
    let mut live_fee_terms = Vec::new();
    let mut source_health = BTreeMap::new();
    let mut expiries = Vec::new();
    let mut no_route_details = Vec::new();
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
            let result = tokio::time::timeout(std::time::Duration::from_secs(4), read)
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
            if let Some(reason) = apply_liquidity(
                &mut source,
                evidence,
                quote.selected_gateway_id.as_deref(),
                amount,
                &mut quote_observations,
            ) {
                no_route_details.push(format!("{}: {reason}", source.id));
                continue;
            }
            source_health.insert(source.id.clone(), health_observation(&observation.health));
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
        let mut source = source;
        let evidence = probe_reads.remove(&source.id);
        if let Some(reason) =
            apply_liquidity(&mut source, evidence, None, amount, &mut quote_observations)
        {
            no_route_details.push(format!("{}: {reason}", source.id));
            continue;
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
        source_health.insert(
            source.id.clone(),
            health_observation(&source.evidence.health),
        );
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
    if live_sources.is_empty() {
        if no_route_details.is_empty() {
            no_route_details.push("No source connectors can quote an executable route".into());
        }
        return Err(LiveNoRoute {
            details: no_route_details,
            quote_observations,
        });
    }

    let terminal_id = ConnectorId::new(format!(
        "lightning:{}",
        stable_destination_id(invoice.as_str())
    ))
    .expect("hashed destination identity is a valid connector ID");
    let terminal = ConnectorSnapshot {
        id: terminal_id.clone(),
        connector_type: ConnectorType::Lightning,
        capabilities: ConnectorCapabilities::new(false, true, false, false),
        liquidity: Evidence::Unknown,
        fee: Evidence::Unknown,
        reliability: Evidence::Unknown,
        evidence: ConnectorEvidence::new(
            terminal_id.clone(),
            None,
            Evidence::Unknown,
            Evidence::Unknown,
            Evidence::Unknown,
        ),
    };

    let mut registry = ConnectorRegistry::default();
    let mut updates = live_sources
        .iter()
        .cloned()
        .map(|snapshot| DiscoveredConnector {
            health: *source_health
                .get(&snapshot.id)
                .expect("every live source has a health observation"),
            snapshot,
        })
        .collect::<Vec<_>>();
    updates.push(DiscoveredConnector {
        snapshot: terminal,
        health: HealthObservation::unknown(),
    });
    registry
        .apply_incremental(updates)
        .map_err(|error| LiveNoRoute {
            details: vec![format!(
                "live graph registry rejected connector data: {error}"
            )],
            quote_observations: Vec::new(),
        })?;
    let registry = registry.snapshot();
    let mut graph = GraphBuilder::new(
        (0..registry.len())
            .map(|index| {
                let connector = registry
                    .get(ecashmesh_core::CompactConnectorId::from_index(
                        u32::try_from(index).unwrap_or(u32::MAX),
                    ))
                    .expect("registry length bounds compact index");
                GraphNode::new(
                    connector.connector_type,
                    connector.capabilities,
                    connector.health,
                )
            })
            .collect(),
    )
    .registry_generation(registry.generation());
    let terminal_compact = registry
        .get(ecashmesh_core::CompactConnectorId::from_index(0))
        .and_then(|_| {
            // ConnectorRegistry intentionally resolves external IDs only in the
            // control plane. Rebuild the deterministic ID relationship here.
            (0..registry.len()).find_map(|index| {
                let compact =
                    ecashmesh_core::CompactConnectorId::from_index(u32::try_from(index).ok()?);
                (registry.get(compact)?.connector_id == terminal_id).then_some(compact)
            })
        })
        .expect("terminal is registered");
    let mut source_endpoints = Vec::new();
    for source in &live_sources {
        let source_compact = (0..registry.len())
            .find_map(|index| {
                let compact =
                    ecashmesh_core::CompactConnectorId::from_index(u32::try_from(index).ok()?);
                (registry.get(compact)?.connector_id == source.id).then_some(compact)
            })
            .expect("source is registered");
        source_endpoints.push(SearchEndpoint::Connector(source_compact));
        let fee = source
            .fee
            .value()
            .map_or(Amount::ZERO, |quote| quote.amount);
        graph.add_edge(ExecutableEdge {
            from: source_compact,
            to: terminal_compact,
            mechanism: if source.connector_type == ConnectorType::Fedimint {
                TransferMechanism::FedimintLightning
            } else {
                TransferMechanism::CashuLightning
            },
            base_fee: fee,
            fee_parts_per_million: 0,
            // A quote proves this exact mechanism was quotable, but not that
            // the mint has spendable reserves or user-held input proofs.
            amount_evidence: AmountAwareEvidence::unknown(),
            execution_evidence: EvidenceState::Known,
        });
    }
    let snapshot = Arc::new(graph.build(1).map_err(|error| LiveNoRoute {
        details: vec![format!("live graph build failed: {error}")],
        quote_observations: Vec::new(),
    })?);
    let publisher = Arc::new(GraphSnapshotPublisher::new(Arc::clone(&snapshot)));
    let router = ScalableRouter::new(publisher, RouteSearchConfig::production_default());
    let result = router
        .evaluate_at(
            &RouteSearchRequest {
                amount,
                sources: source_endpoints,
                destinations: vec![SearchEndpoint::Connector(terminal_compact)],
                // One independent native-settlement candidate per live source.
                // Preserve every viable alternative for registry comparison.
                top_k: live_sources.len().max(1),
            },
            &registry,
            EvidenceTimestamp::from_unix_seconds(unix_now()),
        )
        .map_err(|error| LiveNoRoute {
            details: vec![format!("route search: {error}")],
            quote_observations: Vec::new(),
        })?;
    if result.ranking.ranked.is_empty() {
        return Err(LiveNoRoute {
            details: no_route_details
                .into_iter()
                .chain(std::iter::once(
                    "All quote-backed candidates were rejected during feasibility evaluation"
                        .into(),
                ))
                .collect(),
            quote_observations,
        });
    }
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
        ranking: result.ranking,
        connectors: live_sources,
        quote_observations,
        fee_terms: live_fee_terms,
        expires_at_unix_seconds,
        graph_context: json!({
            "destination": destination_context,
            "sources": source_context,
            "graph_version": result.graph_version,
            "connector_count": snapshot.connector_count(),
            "edge_count": snapshot.edge_count(),
            "mechanisms": mechanisms,
            "search": {
                "candidates_generated": result.metrics.candidates_generated,
                "nodes_explored": result.metrics.nodes_explored,
                "edges_explored": result.metrics.edges_explored,
                "pareto_pruned": result.metrics.pareto_pruned,
                "feasibility_pruned": result.metrics.feasibility_pruned,
                "cache_hit": result.metrics.cache_hit,
            },
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

/// Applies regtest-lab liquidity evidence to one quote-backed source and
/// records it. Returns an exclusion reason when the source cannot route the
/// amount. Fedimint evidence must come from the gateway the quote selected.
fn apply_liquidity(
    source: &mut ConnectorSnapshot,
    evidence: Option<SourceLiquidity>,
    selected_gateway: Option<&str>,
    amount: Amount,
    observations: &mut Vec<Value>,
) -> Option<String> {
    let SourceLiquidity { probe, channels } = evidence?;
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
        crate::probe::combined_effect(probe.as_ref(), channels.as_ref(), amount, now);
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
    exclusion
}

/// Real payment outcomes become both the hop's and the connector's
/// reliability (the model keeps the two levels separate; in this single-hop
/// graph both describe payments through this source). The earliest recorded
/// observation is the connector's first-observed time.
fn apply_reliability(source: &mut ConnectorSnapshot, stats: &crate::history::ReliabilityStats) {
    let evidence = crate::history::evidence(stats);
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

fn health_observation(evidence: &Evidence<ConnectorHealth>) -> HealthObservation {
    let health = |health| match health {
        ConnectorHealth::Healthy => HealthState::Healthy,
        ConnectorHealth::Degraded => HealthState::Degraded,
        ConnectorHealth::Unavailable => HealthState::Unavailable,
    };
    match evidence {
        Evidence::Known(observation) => {
            HealthObservation::known(health(observation.value), observation.observed_at)
        }
        Evidence::Stale(observation) => HealthObservation {
            state: EvidenceState::Stale,
            health: health(observation.value),
            observed_at: Some(observation.observed_at),
        },
        Evidence::Unknown => HealthObservation::unknown(),
    }
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

fn stable_destination_id(value: &str) -> String {
    // Identifier only; this is neither an invoice hash nor a payment secret.
    let mut hash = 1_469_598_103_934_665_603_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    format!("{hash:016x}")
}
