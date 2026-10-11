//! Moving the wallet's own funds between its sources, as an explicit action
//! the user confirms: "move 50,000 sats from Mint A to Federation X".
//!
//! This is the safe form of routing value through another ecash system: the
//! user chooses both ends, sees the fee first, and the funds end up where
//! they asked instead of passing through a custodian hidden inside a payment.
//! It only plans. The wallet then pays the plan's invoice from the source
//! (Cashu melt with `source_melt_quote_id`, or its Fedimint client) and, for
//! a Cashu destination, claims the new ecash with `destination_mint_quote_id`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Error, EvaluateRequest, EvaluateResponse, evaluate, request::DestinationInput, sources::Sources,
};

/// Where the funds leave from.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransferSource {
    CashuMint { url: String },
    Federation { connector_id: String },
}

/// Where the funds arrive. A federation receives through an invoice the
/// wallet creates with its own Fedimint client.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransferTarget {
    CashuMint { url: String },
    Invoice { invoice: String },
}

#[derive(Clone, Debug, Deserialize)]
pub struct TransferRequest {
    /// Sats that arrive at the destination; fees are paid on top.
    pub amount: u64,
    pub from: TransferSource,
    pub to: TransferTarget,
}

/// What the user confirms, and what the wallet needs to execute it.
#[derive(Serialize)]
pub struct TransferPlan {
    pub amount_sats: u64,
    pub source_id: String,
    /// The invoice the source pays.
    pub invoice: String,
    /// Cashu source: melt this quote.
    pub source_melt_quote_id: Option<String>,
    /// Cashu destination: claim the minted ecash with this quote once paid.
    pub destination_mint_quote_id: Option<String>,
    pub destination_mint_url: Option<String>,
    /// Cashu: the mint's fee reserve (an upper bound; the rest returns as
    /// change). Fedimint: the quoted fee.
    pub estimated_fee_sats: Option<u64>,
    pub fee_kind: &'static str,
    pub expires_at_unix_seconds: u64,
    /// The full evaluation behind the plan, for its explanation.
    pub evaluation: EvaluateResponse,
}

pub(crate) async fn plan(
    sources: &Sources,
    request: TransferRequest,
) -> Result<TransferPlan, Error> {
    if let (TransferSource::CashuMint { url: from }, TransferTarget::CashuMint { url: to }) =
        (&request.from, &request.to)
    {
        let canonical = ecashmesh_cashu::discovery::canonical_mint_url;
        if canonical(from).ok() == canonical(to).ok() {
            return Err(Error::validation(
                "A transfer needs two different sources",
                vec!["to".into()],
            ));
        }
    }
    let destination = match &request.to {
        TransferTarget::CashuMint { url } => DestinationInput {
            kind: "cashu".into(),
            value: format!(
                "cashu://request?mint={}&amount_sats={}",
                percent_encode(url),
                request.amount
            ),
            mint_url: None,
        },
        TransferTarget::Invoice { invoice } => DestinationInput {
            kind: "lightning".into(),
            value: invoice.clone(),
            mint_url: None,
        },
    };
    let (wallet_mint_urls, source_mint_url, federation_connector_ids) = match request.from {
        TransferSource::CashuMint { url } => (vec![url.clone()], Some(url), Vec::new()),
        TransferSource::Federation { connector_id } => (Vec::new(), None, vec![connector_id]),
    };
    let evaluation = evaluate::evaluate(
        sources,
        EvaluateRequest {
            amount: request.amount,
            asset: "BTC".into(),
            destination,
            payment_intent: "send".into(),
            candidate_connectors: Vec::new(),
            source_connector: None,
            source_mint_url,
            wallet_mint_urls,
            wallet_mint_balances: std::collections::BTreeMap::new(),
            federation_connector_ids,
            federation_identities: std::collections::BTreeMap::new(),
            // Only the chosen source may pay, never a discovered one.
            strict_source_registry: true,
            mint_urls: Vec::new(),
        },
    )
    .await?;

    let source = &evaluation.recommended_source;
    let quotes = evaluation
        .live
        .as_ref()
        .and_then(|live| live["quote_observations"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let known = |kind: &str, connector: Option<&str>| {
        quotes.iter().find(|quote| {
            quote["kind"] == kind
                && quote["state"] == "known"
                && connector.is_none_or(|id| quote["connector"] == id)
        })
    };
    let destination_quote = known("destination_mint_quote", None);
    let invoice = match (&request.to, destination_quote) {
        (TransferTarget::Invoice { invoice }, _) => invoice.clone(),
        (TransferTarget::CashuMint { .. }, Some(quote)) => text(&quote["value"]["invoice"])
            .ok_or_else(|| {
                Error::no_viable("The destination mint quote has no invoice", Vec::new())
            })?,
        (TransferTarget::CashuMint { .. }, None) => {
            return Err(Error::no_viable(
                "The destination mint did not return a quote",
                Vec::new(),
            ));
        }
    };
    Ok(TransferPlan {
        amount_sats: request.amount,
        source_id: source.source_id.clone(),
        invoice,
        source_melt_quote_id: known("source_melt_quote", Some(&source.source_id))
            .and_then(|quote| text(&quote["value"]["quote_id"])),
        destination_mint_quote_id: destination_quote
            .and_then(|quote| text(&quote["value"]["quote_id"])),
        destination_mint_url: destination_quote.and_then(|quote| text(&quote["mint_url"])),
        estimated_fee_sats: source.fee.estimated_fee_sats,
        fee_kind: source.fee.estimate_kind,
        expires_at_unix_seconds: evaluation.expires_at_unix_seconds,
        evaluation,
    })
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(ToOwned::to_owned)
}

/// Percent-encodes a URL for a query value (RFC 3986 unreserved kept).
fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;
    value.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_urls_survive_the_payment_request_round_trip() {
        let url = "https://mint.example/cashu/api";
        let request = ecashmesh_cashu::CashuPaymentRequest::parse(&format!(
            "cashu://request?mint={}&amount_sats=5",
            percent_encode(url)
        ))
        .unwrap();
        assert_eq!(request.mint_urls.len(), 1);
        assert_eq!(
            ecashmesh_cashu::discovery::canonical_mint_url(&request.mint_urls[0]),
            ecashmesh_cashu::discovery::canonical_mint_url(url)
        );
    }
}
