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
    Amount, AmountAwareEvidence, ConnectorCapabilities, ConnectorEvidence, ConnectorHealth,
    ConnectorId, ConnectorRegistry, ConnectorSnapshot, ConnectorType, DiscoveredConnector,
    Evidence, EvidenceState, EvidenceTimestamp, ExecutableEdge, GraphBuilder, GraphNode,
    GraphSnapshotPublisher, HealthObservation, HealthState, LightningInvoice, RouteRanking,
    RouteSearchConfig, RouteSearchRequest, ScalableRouter, SearchEndpoint, TransferMechanism,
};
use serde_json::{Value, json};

use crate::connectors::{ConnectorBatch, unix_now};

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
}

/// Quote and keyset-fee facts for a quoted source connector.
pub(super) struct LiveFeeTerms {
    /// Source connector that returned the NUT-05 quote.
    pub source_connector: ConnectorId,
    /// NUT-05 upper-bound reserve, not a final paid fee.
    pub fee_reserve_sats: Amount,
    /// NUT-02 keyset schedule. It cannot be totalled until actual proofs are
    /// selected, so it is deliberately excluded from the quote estimate.
    pub input_fees: Evidence<Vec<KeysetFee>>,
}

/// A real-data failure that deliberately does not manufacture a candidate.
pub(super) struct LiveNoRoute {
    pub details: Vec<String>,
    pub quote_observations: Vec<Value>,
}

/// Builds only declared Cashu-to-Lightning edges for the normalized destination
/// and evaluates the resulting immutable graph through the Phase 9 router.
#[allow(clippy::too_many_lines)] // Keeps the full live feasibility chain and its exact no-route diagnostics together.
pub(super) async fn evaluate(
    service: &Arc<DiscoveryService>,
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
    if let Some(expiry) = destination_expiry {
        expiries.push(expiry);
    }
    for source in sources {
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
        let quote = service
            .melt_quote(source.id.clone(), &observation.mint_url, &invoice, amount)
            .await
            .map_err(|error| LiveNoRoute {
                details: vec![format!("{}: source melt quote failed: {error}", source.id)],
                quote_observations: Vec::new(),
            })?;
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
        source.fee = Evidence::reported(
            ecashmesh_core::FeeQuote::new(quote_evidence.value.fee_reserve_sats),
            quote_evidence.source,
            quote_evidence.observed_at,
            quote_evidence.confidence,
        );
        live_fee_terms.push(LiveFeeTerms {
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
            mechanism: TransferMechanism::CashuLightning,
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
                top_k: 3,
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
    Ok(LiveEvaluation {
        ranking: result.ranking,
        connectors: live_sources,
        quote_observations,
        fee_terms: live_fee_terms,
        expires_at_unix_seconds,
        graph_context: json!({
            "destination": destination_context,
            "graph_version": result.graph_version,
            "connector_count": snapshot.connector_count(),
            "edge_count": snapshot.edge_count(),
            "mechanisms": ["cashu_lightning"],
            "search": {
                "candidates_generated": result.metrics.candidates_generated,
                "nodes_explored": result.metrics.nodes_explored,
                "edges_explored": result.metrics.edges_explored,
                "pareto_pruned": result.metrics.pareto_pruned,
                "feasibility_pruned": result.metrics.feasibility_pruned,
                "cache_hit": result.metrics.cache_hit,
            },
            "route_shape": match destination_type {
                "cashu" => "Cashu source → Lightning invoice → Cashu destination quote",
                _ => "Cashu source → Lightning invoice",
            }
        }),
    })
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
