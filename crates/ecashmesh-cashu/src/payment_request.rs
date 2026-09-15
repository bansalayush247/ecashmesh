//! Parsing for the quote-only Cashu destination format accepted by the demo.
//!
//! A live cross-mint evaluation needs an explicit destination mint. The
//! request format is `cashu://request?mint=https%3A%2F%2Fmint.example` with an
//! optional `invoice=` field. When no invoice is attached, `EcashMesh` asks the
//! destination mint for a fresh unpaid NUT-04 quote; it never creates tokens
//! or executes the resulting payment.

use core::fmt;

use ecashmesh_core::{Amount, LightningInvoice};
use reqwest::Url;

use crate::discovery::canonical_mint_url;

/// A normalized Cashu destination request usable by the quote-only live demo.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CashuPaymentRequest {
    /// Canonical destination mint base URL.
    pub mint_url: String,
    /// Optional real invoice embedded by the requesting host.
    pub invoice: Option<LightningInvoice>,
    /// Optional amount claimed by the request itself.
    pub amount: Option<Amount>,
}

impl CashuPaymentRequest {
    /// Parses a destination request without contacting a mint.
    ///
    /// # Errors
    ///
    /// Rejects opaque Cashu tokens, malformed URLs, conflicting duplicate
    /// query fields, unsupported request amounts, and invalid embedded
    /// invoices. Tokens are intentionally not accepted as payment requests.
    pub fn parse(value: &str) -> Result<Self, CashuPaymentRequestError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(CashuPaymentRequestError::Empty);
        }
        let mint_url = value
            .strip_prefix("cashu:")
            .filter(|rest| rest.starts_with("https://") || rest.starts_with("http://"))
            .map(ToOwned::to_owned);
        let (mint_url, invoice, amount) = if let Some(mint_url) = mint_url {
            (mint_url, None, None)
        } else {
            let url = Url::parse(value).map_err(|_| CashuPaymentRequestError::InvalidFormat)?;
            if url.scheme() != "cashu" {
                return Err(CashuPaymentRequestError::InvalidFormat);
            }
            let pairs = url.query_pairs().collect::<Vec<_>>();
            let mint = one_query_value(&pairs, "mint")?
                .or_else(|| {
                    url.host_str().and_then(|host| {
                        (host != "request").then(|| format!("https://{host}{}", url.path()))
                    })
                })
                .ok_or(CashuPaymentRequestError::MissingMint)?;
            let invoice = one_query_value(&pairs, "invoice")?
                .map(|invoice| LightningInvoice::parse(&invoice))
                .transpose()
                .map_err(CashuPaymentRequestError::InvalidInvoice)?;
            let amount = one_query_value(&pairs, "amount_sats")?
                .or(one_query_value(&pairs, "amount")?)
                .map(|amount| {
                    amount
                        .parse::<u64>()
                        .map(Amount::from_sats)
                        .map_err(|_| CashuPaymentRequestError::InvalidAmount)
                })
                .transpose()?;
            (mint, invoice, amount)
        };
        Ok(Self {
            mint_url: canonical_mint_url(&mint_url)
                .map_err(|_| CashuPaymentRequestError::InvalidMintUrl)?,
            invoice,
            amount,
        })
    }
}

/// Cashu destination-request parsing failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CashuPaymentRequestError {
    /// The caller supplied no request.
    Empty,
    /// The string is not a supported Cashu request URI.
    InvalidFormat,
    /// The request did not name a destination mint.
    MissingMint,
    /// The named mint URL failed canonical validation.
    InvalidMintUrl,
    /// A repeated query key carried different values.
    ConflictingField,
    /// An attached invoice is malformed.
    InvalidInvoice(ecashmesh_core::LightningInvoiceError),
    /// The request amount is not a non-negative whole-satoshi integer.
    InvalidAmount,
}

impl fmt::Display for CashuPaymentRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Cashu payment request is empty"),
            Self::InvalidFormat => formatter.write_str(
                "Destination is not a supported Cashu request URI; opaque Cashu tokens are not payment requests",
            ),
            Self::MissingMint => formatter.write_str("Cashu payment request does not name a destination mint"),
            Self::InvalidMintUrl => formatter.write_str("Cashu payment request contains an invalid mint URL"),
            Self::ConflictingField => formatter.write_str("Cashu payment request has conflicting query fields"),
            Self::InvalidInvoice(error) => write!(formatter, "Cashu payment request invoice: {error}"),
            Self::InvalidAmount => formatter.write_str("Cashu payment request amount must be whole sats"),
        }
    }
}

impl std::error::Error for CashuPaymentRequestError {}

fn one_query_value(
    pairs: &[(std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)],
    name: &str,
) -> Result<Option<String>, CashuPaymentRequestError> {
    let mut values = pairs
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.to_string());
    let first = values.next();
    if values.any(|value| Some(value) != first) {
        return Err(CashuPaymentRequestError::ConflictingField);
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_mint_request_without_contacting_the_mint() {
        let request = CashuPaymentRequest::parse(
            "cashu://request?mint=https%3A%2F%2Fmint.example%2Fapi&amount_sats=100000",
        )
        .unwrap();
        assert_eq!(request.mint_url, "https://mint.example/api/");
        assert_eq!(request.amount, Some(Amount::from_sats(100_000)));
        assert!(request.invoice.is_none());
    }

    #[test]
    fn does_not_treat_a_cashu_token_as_a_payment_request() {
        assert_eq!(
            CashuPaymentRequest::parse("cashuAeyJ0b2tlbiI6W119").unwrap_err(),
            CashuPaymentRequestError::InvalidFormat
        );
    }
}
