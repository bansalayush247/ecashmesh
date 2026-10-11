//! Split payments: pay one invoice from several Cashu mints when no single
//! source has enough.
//!
//! Each mint pays a part of the same invoice (NUT-15 partial melt); the
//! receiver settles only once every part arrived, so Lightning makes the
//! payment atomic: all parts settle or all fail and the ecash stays with
//! the wallet. Fedimint sends always pay the full invoice amount, so only
//! mints advertising NUT-15 can take a part. This module only plans: it
//! returns one melt quote per part, which the wallet executes together.

use std::{cmp::Reverse, collections::BTreeMap, str::FromStr, time::Duration};

use ecashmesh_cashu::discovery::{DiscoverySource, MintHint, canonical_mint_url};
use ecashmesh_core::Amount;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Error,
    live::{self, LiveDestination},
    request::DestinationInput,
    sources::{Sources, unix_now},
};

const QUOTE_TIMEOUT: Duration = Duration::from_secs(5);

/// What to pay, and how much ecash the wallet holds at each mint.
#[derive(Clone, Debug, Deserialize)]
pub struct SplitRequest {
    pub amount: u64,
    pub destination: DestinationInput,
    /// Spendable balance in sats per Cashu mint URL. Only these mints pay.
    pub balances: BTreeMap<String, u64>,
}

/// One mint's share: melt `amount_sats` with `melt_quote_id`.
#[derive(Clone, Debug, Serialize)]
pub struct SplitPart {
    pub source_id: String,
    pub mint_url: String,
    pub amount_sats: u64,
    pub melt_quote_id: String,
    /// Upper bound the mint reserves for Lightning fees; unused reserve is
    /// returned as change.
    pub fee_reserve_sats: u64,
    pub expires_at_unix_seconds: Option<u64>,
}

/// A mint that holds a balance but takes no part, and why.
#[derive(Clone, Debug, Serialize)]
pub struct SkippedSource {
    pub source_id: Option<String>,
    pub mint_url: String,
    pub reason: String,
}

/// Parts that together pay the invoice exactly.
#[derive(Clone, Debug, Serialize)]
pub struct SplitPlan {
    pub amount_sats: u64,
    /// The invoice every part pays.
    pub invoice: String,
    pub parts: Vec<SplitPart>,
    pub total_fee_reserve_sats: u64,
    /// Execute every part before this; quotes and the invoice expire.
    pub expires_at_unix_seconds: u64,
    /// All parts settle or none do.
    pub atomic: bool,
    pub skipped: Vec<SkippedSource>,
    pub destination: Value,
}

/// A mint that may take a part.
struct Candidate<'a> {
    observation: &'a ecashmesh_cashu::CashuObservation,
    balance: u64,
}

#[allow(clippy::too_many_lines)] // Validation, candidate selection and allocation read top to bottom.
pub(crate) async fn plan(sources: &Sources, request: SplitRequest) -> Result<SplitPlan, Error> {
    if request.amount == 0 {
        return Err(Error::validation(
            "amount must be greater than zero",
            vec!["amount".into()],
        ));
    }
    let destination = LiveDestination::parse(&request.destination.kind, &request.destination.value)
        .map_err(|error| Error::validation("Unsupported destination", vec![error]))?;
    let mut balances = BTreeMap::new();
    for (url, balance) in request.balances {
        let url = canonical_mint_url(&url).map_err(|error| {
            Error::validation("balances contains an invalid mint URL", vec![error])
        })?;
        balances.insert(url, balance);
    }
    let destination_mints = destination
        .destination_mint_urls()
        .into_iter()
        .filter_map(|url| canonical_mint_url(url).ok())
        .collect::<Vec<_>>();
    let hints = balances
        .keys()
        .map(|url| (url.clone(), DiscoverySource::Wallet))
        .chain(
            destination_mints
                .iter()
                .map(|url| (url.clone(), DiscoverySource::Destination)),
        )
        .map(|(url, source)| MintHint {
            url,
            alias: None,
            source,
            observed_at: unix_now(),
            stale: false,
        })
        .collect();
    let amount = Amount::from_sats(request.amount);
    let batch = sources.collect(amount, hints).await?;
    let mut observations = Vec::new();
    let resolved = live::resolve_destination(
        sources.cashu_service(),
        &batch,
        destination,
        amount,
        &mut observations,
    )
    .await
    .map_err(|details| Error::no_viable("The destination has no payable invoice.", details))?;
    if !accepts_multipath(resolved.invoice.as_str()) {
        return Err(Error::no_viable(
            "The destination invoice does not accept multi-path payments, so it cannot be split.",
            Vec::new(),
        ));
    }

    let mut skipped = Vec::new();
    let mut candidates = Vec::new();
    for (url, &balance) in &balances {
        let observation = batch
            .live_observations
            .iter()
            .find(|observation| canonical_mint_url(&observation.mint_url).as_ref() == Ok(url));
        let reason = match observation {
            None => Some("The mint could not be observed".to_owned()),
            Some(_) if balance == 0 => Some("No balance at this mint".into()),
            Some(_) if destination_mints.contains(url) => {
                Some("This mint is the destination".into())
            }
            Some(observation) if !observation.supports_partial_payments() => {
                Some("The mint does not support partial payments (NUT-15)".into())
            }
            Some(_) => None,
        };
        match (reason, observation) {
            (None, Some(observation)) => candidates.push(Candidate {
                observation,
                balance,
            }),
            (reason, observation) => skipped.push(SkippedSource {
                source_id: observation.map(|observation| observation.id.to_string()),
                mint_url: url.clone(),
                reason: reason.unwrap_or_default(),
            }),
        }
    }
    // Largest balances first: the fewest parts, each with a fee reserve.
    candidates
        .sort_by_key(|candidate| (Reverse(candidate.balance), candidate.observation.id.clone()));

    let mut remaining = request.amount;
    let mut parts = Vec::new();
    for candidate in candidates {
        if remaining == 0 {
            break;
        }
        match quote_part(
            sources,
            &resolved.invoice,
            &candidate,
            remaining,
            request.amount,
        )
        .await
        {
            Ok(part) => {
                remaining -= part.amount_sats;
                parts.push(part);
            }
            Err(reason) => skipped.push(SkippedSource {
                source_id: Some(candidate.observation.id.to_string()),
                mint_url: candidate.observation.mint_url.clone(),
                reason,
            }),
        }
    }
    if remaining > 0 {
        let mut error = Error::no_viable(
            format!(
                "Mints that support partial payments can cover {} of {} sats.",
                request.amount - remaining,
                request.amount
            ),
            skipped
                .iter()
                .map(|skip| format!("{}: {}", skip.mint_url, skip.reason))
                .collect(),
        );
        error.diagnostics = Some(json!({ "skipped": skipped, "quote_observations": observations }));
        return Err(error);
    }
    Ok(SplitPlan {
        amount_sats: request.amount,
        invoice: resolved.invoice.as_str().to_owned(),
        total_fee_reserve_sats: parts.iter().map(|part| part.fee_reserve_sats).sum(),
        expires_at_unix_seconds: parts
            .iter()
            .filter_map(|part| part.expires_at_unix_seconds)
            .chain(resolved.expiry)
            .min()
            .unwrap_or(batch.expires_at_unix_seconds),
        parts,
        atomic: true,
        skipped,
        destination: resolved.context,
    })
}

/// Quotes the largest part this mint can pay: its balance must cover the
/// part plus the fee reserve the mint quotes for it.
async fn quote_part(
    sources: &Sources,
    invoice: &ecashmesh_core::LightningInvoice,
    candidate: &Candidate<'_>,
    remaining: u64,
    total: u64,
) -> Result<SplitPart, String> {
    let observation = candidate.observation;
    let mut part = remaining.min(candidate.balance);
    // The reserve depends on the part, so one re-quote may be needed after
    // shrinking the part to make room for it.
    for _ in 0..2 {
        if let Some(reason) = observation.send_unavailable_reason(Amount::from_sats(part)) {
            return Err(reason.into());
        }
        let service = sources.cashu_service();
        let (id, url, amount) = (
            observation.id.clone(),
            &observation.mint_url,
            Amount::from_sats(part),
        );
        let request = async {
            // A single part paying the whole invoice is an ordinary melt.
            if part == total {
                service.melt_quote(id, url, invoice, amount).await
            } else {
                service.partial_melt_quote(id, url, invoice, amount).await
            }
        };
        let quote = tokio::time::timeout(QUOTE_TIMEOUT, request)
            .await
            .map_err(|_| "The melt quote timed out".to_owned())??;
        let Some(melt) = quote.evidence.value() else {
            return Err(quote.issue.map_or_else(
                || "The mint returned no usable melt quote".into(),
                |issue| format!("{}: {}", issue.code, issue.message),
            ));
        };
        let fee = melt.fee_reserve_sats.sats();
        if part.saturating_add(fee) <= candidate.balance {
            return Ok(SplitPart {
                source_id: observation.id.to_string(),
                mint_url: observation.mint_url.clone(),
                amount_sats: part,
                melt_quote_id: melt.quote_id.clone(),
                fee_reserve_sats: fee,
                expires_at_unix_seconds: melt.expires_at_unix_seconds,
            });
        }
        let room = candidate.balance.saturating_sub(fee);
        if room == 0 || room >= part {
            break;
        }
        part = room;
    }
    Err("The balance cannot cover a part plus its fee reserve".into())
}

/// BOLT11 feature `basic_mpp`: the receiver accepts a payment in parts.
#[allow(clippy::redundant_closure_for_method_calls)] // The method path is in an undeclared crate.
fn accepts_multipath(invoice: &str) -> bool {
    lightning_invoice::Bolt11Invoice::from_str(invoice)
        .ok()
        .and_then(|invoice| {
            invoice
                .features()
                .map(|features| features.supports_basic_mpp())
        })
        .unwrap_or(false)
}
