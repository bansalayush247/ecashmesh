//! Parsing for Cashu destination payment requests.
//!
//! The live demo accepts the original `cashu://request?...` URI and NUT-18
//! `creqA...` requests. NUT-18 requests are CBOR encoded and base64url
//! serialized. This module only parses requests: it never accepts bearer
//! tokens, proofs, or custody material.

use core::fmt;
use std::collections::BTreeSet;

use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use ecashmesh_core::{Amount, LightningInvoice};
use reqwest::Url;

use crate::discovery::canonical_mint_url;

const MAX_NUT18_PAYLOAD_BYTES: usize = 16_384;
const MAX_NUT18_DEPTH: usize = 16;
const MAX_NUT18_ITEMS: usize = 256;

/// The source serialization accepted for a normalized Cashu request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CashuPaymentRequestEncoding {
    /// The original URI used by the reference demo.
    Uri,
    /// A NUT-18 CBOR/base64url `creqA...` request.
    Nut18,
}

impl CashuPaymentRequestEncoding {
    /// Stable code for API inspection.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Uri => "cashu_uri",
            Self::Nut18 => "nut18_creq",
        }
    }
}

/// A NUT-18 requested transport. An empty list represents NUT-18 in-band
/// delivery, which this read-only reference client cannot execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CashuTransport {
    /// NUT-18 transport type, such as `nostr` or `post`.
    pub kind: String,
    /// Transport target supplied by the requesting wallet.
    pub target: String,
}

/// A NUT-18 supported payment method and its receiver-requested surcharge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CashuSupportedMethod {
    /// Method name, for example `bolt11`.
    pub method: String,
    /// Optional receiver surcharge in the request unit. This is not a Cashu
    /// melt reserve or a keyset input fee.
    pub fee: Option<Amount>,
}

/// A normalized Cashu destination request usable by quote-only live routing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CashuPaymentRequest {
    /// Canonical primary destination mint. It is the first NUT-18 mint after
    /// canonical de-duplication, retaining NUT-18 request order.
    pub mint_url: String,
    /// Every canonical mint accepted or preferred by the receiver.
    pub mint_urls: Vec<String>,
    /// Whether the NUT-18 mint list is advisory rather than strict.
    pub mints_are_preferred: bool,
    /// Optional Lightning invoice embedded by the requester.
    ///
    /// NUT-18 itself does not standardize an invoice field. `EcashMesh` accepts
    /// the explicit `invoice`/`bolt11` extension, and a `bolt11` transport
    /// target, when present so a host can avoid an extra destination quote.
    pub invoice: Option<LightningInvoice>,
    /// Optional payment amount requested by the receiver, net of NUT-02 input
    /// fees as defined by NUT-18.
    pub amount: Option<Amount>,
    /// NUT-18 unit, when supplied. Live BTC routing only accepts `sat`.
    pub unit: Option<String>,
    /// Receiver-requested delivery transports in request order.
    pub transports: Vec<CashuTransport>,
    /// Receiver-supported melt-out methods and their independent surcharges.
    pub supported_methods: Vec<CashuSupportedMethod>,
    /// Original request encoding for inspectable normalization.
    pub encoding: CashuPaymentRequestEncoding,
}

impl CashuPaymentRequest {
    /// Parses a destination request without contacting a mint.
    ///
    /// # Errors
    ///
    /// Rejects bearer Cashu tokens, malformed or unsupported NUT-18 requests,
    /// ambiguous fields, invalid mint URLs, and invalid embedded invoices.
    pub fn parse(value: &str) -> Result<Self, CashuPaymentRequestError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(CashuPaymentRequestError::Empty);
        }
        if value.starts_with("creqA") {
            return Self::parse_nut18(value);
        }
        if value.starts_with("cashuA") || value.starts_with("cashuB") {
            return Err(CashuPaymentRequestError::BearerToken);
        }
        Self::parse_uri(value)
    }

    fn parse_uri(value: &str) -> Result<Self, CashuPaymentRequestError> {
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
                .map(|amount| parse_amount(&amount))
                .transpose()?;
            (mint, invoice, amount)
        };
        Self::from_parts(
            vec![mint_url],
            false,
            invoice,
            amount,
            Some("sat".into()),
            Vec::new(),
            Vec::new(),
            CashuPaymentRequestEncoding::Uri,
        )
    }

    fn parse_nut18(value: &str) -> Result<Self, CashuPaymentRequestError> {
        let payload = decode_base64url(&value[5..])?;
        let request = CborDecoder::new(&payload).decode()?;
        let fields = request.as_map()?;
        let amount = optional_unsigned(fields, "a")?.map(Amount::from_sats);
        let unit = optional_text(fields, "u")?;
        if (amount.is_some() || fields.contains_key("sm")) && unit.is_none() {
            return Err(CashuPaymentRequestError::MissingUnit);
        }
        if let Some(unit) = &unit
            && unit != "sat"
        {
            return Err(CashuPaymentRequestError::UnsupportedUnit(unit.clone()));
        }
        let mint_urls = required_text_array(fields, "m")?;
        let mints_are_preferred = optional_bool(fields, "mp")?.unwrap_or(false);
        let transports = optional_transports(fields)?;
        let supported_methods = optional_supported_methods(fields)?;
        let invoice = optional_invoice(fields, &transports)?;
        Self::from_parts(
            mint_urls,
            mints_are_preferred,
            invoice,
            amount,
            unit,
            transports,
            supported_methods,
            CashuPaymentRequestEncoding::Nut18,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        mint_urls: Vec<String>,
        mints_are_preferred: bool,
        invoice: Option<LightningInvoice>,
        amount: Option<Amount>,
        unit: Option<String>,
        transports: Vec<CashuTransport>,
        supported_methods: Vec<CashuSupportedMethod>,
        encoding: CashuPaymentRequestEncoding,
    ) -> Result<Self, CashuPaymentRequestError> {
        let mut canonical_mints = Vec::new();
        let mut seen = BTreeSet::new();
        for mint in mint_urls {
            let canonical =
                canonical_mint_url(&mint).map_err(|_| CashuPaymentRequestError::InvalidMintUrl)?;
            if seen.insert(canonical.clone()) {
                canonical_mints.push(canonical);
            }
        }
        let mint_url = canonical_mints
            .first()
            .cloned()
            .ok_or(CashuPaymentRequestError::MissingMint)?;
        Ok(Self {
            mint_url,
            mint_urls: canonical_mints,
            mints_are_preferred,
            invoice,
            amount,
            unit,
            transports,
            supported_methods,
            encoding,
        })
    }
}

/// Cashu destination-request parsing failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CashuPaymentRequestError {
    /// The caller supplied no request.
    Empty,
    /// The value is an opaque bearer Cashu token, not a payment request.
    BearerToken,
    /// The string is not a supported URI or NUT-18 `creqA...` request.
    InvalidFormat,
    /// NUT-18 base64url decoding failed.
    InvalidNut18Encoding,
    /// NUT-18 CBOR is malformed or exceeds safe structural bounds.
    MalformedNut18,
    /// The request did not name a destination mint.
    MissingMint,
    /// NUT-18 requires a unit whenever it has amount or supported methods.
    MissingUnit,
    /// The live BTC/sat route evaluator cannot convert the claimed unit.
    UnsupportedUnit(String),
    /// The named mint URL failed canonical validation.
    InvalidMintUrl,
    /// A repeated URI or CBOR field carried conflicting values.
    ConflictingField,
    /// A NUT-18 field had the wrong CBOR type or invalid contents.
    InvalidNut18Field(&'static str),
    /// An attached invoice is malformed.
    InvalidInvoice(ecashmesh_core::LightningInvoiceError),
    /// The request amount is not a non-negative whole-satoshi integer.
    InvalidAmount,
}

impl fmt::Display for CashuPaymentRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Cashu payment request is empty"),
            Self::BearerToken => formatter.write_str(
                "Cashu bearer tokens are not payment requests and cannot be used as a destination",
            ),
            Self::InvalidFormat => {
                formatter.write_str("Destination is not a supported Cashu payment request URI")
            }
            Self::InvalidNut18Encoding => {
                formatter.write_str("NUT-18 Cashu payment request has invalid base64url encoding")
            }
            Self::MalformedNut18 => formatter
                .write_str("NUT-18 Cashu payment request has malformed or unsupported CBOR data"),
            Self::MissingMint => {
                formatter.write_str("Cashu payment request does not name a destination mint")
            }
            Self::MissingUnit => formatter.write_str(
                "NUT-18 Cashu payment request requires a unit when amount or methods are present",
            ),
            Self::UnsupportedUnit(unit) => {
                write!(
                    formatter,
                    "NUT-18 Cashu payment request unit is unsupported: {unit}"
                )
            }
            Self::InvalidMintUrl => {
                formatter.write_str("Cashu payment request contains an invalid mint URL")
            }
            Self::ConflictingField => {
                formatter.write_str("Cashu payment request has conflicting duplicate fields")
            }
            Self::InvalidNut18Field(field) => {
                write!(
                    formatter,
                    "NUT-18 Cashu payment request has an invalid {field} field"
                )
            }
            Self::InvalidInvoice(error) => {
                write!(formatter, "Cashu payment request invoice: {error}")
            }
            Self::InvalidAmount => formatter.write_str(
                "Cashu payment request amount must be a non-negative whole-satoshi integer",
            ),
        }
    }
}

impl std::error::Error for CashuPaymentRequestError {}

fn parse_amount(value: &str) -> Result<Amount, CashuPaymentRequestError> {
    value
        .parse::<u64>()
        .map(Amount::from_sats)
        .map_err(|_| CashuPaymentRequestError::InvalidAmount)
}

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

fn decode_base64url(payload: &str) -> Result<Vec<u8>, CashuPaymentRequestError> {
    if payload.is_empty() || payload.len() > MAX_NUT18_PAYLOAD_BYTES * 2 {
        return Err(CashuPaymentRequestError::InvalidNut18Encoding);
    }
    let mut padded = payload.to_owned();
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    let decoded = URL_SAFE
        .decode(padded)
        .map_err(|_| CashuPaymentRequestError::InvalidNut18Encoding)?;
    if decoded.len() > MAX_NUT18_PAYLOAD_BYTES {
        return Err(CashuPaymentRequestError::MalformedNut18);
    }
    Ok(decoded)
}

fn required_text_array(
    fields: &CborMap,
    field: &'static str,
) -> Result<Vec<String>, CashuPaymentRequestError> {
    let value = fields
        .get(field)
        .ok_or(CashuPaymentRequestError::MissingMint)?;
    let values = value.as_array()?;
    if values.is_empty() || values.len() > 32 {
        return Err(CashuPaymentRequestError::InvalidNut18Field(field));
    }
    values
        .iter()
        .map(|value| value.as_text().map(ToOwned::to_owned))
        .collect()
}

fn optional_unsigned(
    fields: &CborMap,
    field: &'static str,
) -> Result<Option<u64>, CashuPaymentRequestError> {
    fields.get(field).map(CborValue::as_unsigned).transpose()
}

fn optional_text(
    fields: &CborMap,
    field: &'static str,
) -> Result<Option<String>, CashuPaymentRequestError> {
    fields
        .get(field)
        .map(|value| value.as_text().map(ToOwned::to_owned))
        .transpose()
}

fn optional_bool(
    fields: &CborMap,
    field: &'static str,
) -> Result<Option<bool>, CashuPaymentRequestError> {
    fields.get(field).map(CborValue::as_bool).transpose()
}

fn optional_transports(fields: &CborMap) -> Result<Vec<CashuTransport>, CashuPaymentRequestError> {
    let Some(value) = fields.get("t") else {
        return Ok(Vec::new());
    };
    let transports = value.as_array()?;
    if transports.len() > 16 {
        return Err(CashuPaymentRequestError::InvalidNut18Field("t"));
    }
    transports
        .iter()
        .map(|transport| {
            let transport = transport.as_map()?;
            let kind = required_text(transport, "t")?;
            let target = required_text(transport, "a")?;
            if kind.is_empty() || target.is_empty() {
                return Err(CashuPaymentRequestError::InvalidNut18Field("t"));
            }
            Ok(CashuTransport { kind, target })
        })
        .collect()
}

fn optional_supported_methods(
    fields: &CborMap,
) -> Result<Vec<CashuSupportedMethod>, CashuPaymentRequestError> {
    let Some(value) = fields.get("sm") else {
        return Ok(Vec::new());
    };
    let methods = value.as_array()?;
    if methods.len() > 16 {
        return Err(CashuPaymentRequestError::InvalidNut18Field("sm"));
    }
    let mut seen = BTreeSet::new();
    methods
        .iter()
        .map(|method| {
            let method = method.as_map()?;
            let name = required_text(method, "mn")?;
            if name.is_empty() || !seen.insert(name.clone()) {
                return Err(CashuPaymentRequestError::InvalidNut18Field("sm"));
            }
            Ok(CashuSupportedMethod {
                method: name,
                fee: optional_unsigned(method, "mf")?.map(Amount::from_sats),
            })
        })
        .collect()
}

fn optional_invoice(
    fields: &CborMap,
    transports: &[CashuTransport],
) -> Result<Option<LightningInvoice>, CashuPaymentRequestError> {
    let extensions = ["invoice", "bolt11"]
        .into_iter()
        .filter_map(|field| fields.get(field).map(|value| value.as_text()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut values = extensions
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    values.extend(
        transports
            .iter()
            .filter(|transport| transport.kind == "bolt11")
            .map(|transport| transport.target.clone()),
    );
    values.sort();
    values.dedup();
    match values.as_slice() {
        [] => Ok(None),
        [invoice] => LightningInvoice::parse(invoice)
            .map(Some)
            .map_err(CashuPaymentRequestError::InvalidInvoice),
        _ => Err(CashuPaymentRequestError::ConflictingField),
    }
}

fn required_text(
    fields: &CborMap,
    field: &'static str,
) -> Result<String, CashuPaymentRequestError> {
    fields
        .get(field)
        .ok_or(CashuPaymentRequestError::InvalidNut18Field(field))?
        .as_text()
        .map(ToOwned::to_owned)
}

type CborMap = std::collections::BTreeMap<String, CborValue>;

#[derive(Clone, Debug, Eq, PartialEq)]
enum CborValue {
    Unsigned(u64),
    Text(String),
    Array(Vec<Self>),
    Map(CborMap),
    Bool(bool),
    Null,
}

impl CborValue {
    fn as_unsigned(&self) -> Result<u64, CashuPaymentRequestError> {
        if let Self::Unsigned(value) = self {
            Ok(*value)
        } else {
            Err(CashuPaymentRequestError::InvalidNut18Field("value"))
        }
    }

    fn as_text(&self) -> Result<&str, CashuPaymentRequestError> {
        if let Self::Text(value) = self {
            Ok(value)
        } else {
            Err(CashuPaymentRequestError::InvalidNut18Field("value"))
        }
    }

    fn as_array(&self) -> Result<&[Self], CashuPaymentRequestError> {
        if let Self::Array(values) = self {
            Ok(values)
        } else {
            Err(CashuPaymentRequestError::InvalidNut18Field("value"))
        }
    }

    fn as_map(&self) -> Result<&CborMap, CashuPaymentRequestError> {
        if let Self::Map(values) = self {
            Ok(values)
        } else {
            Err(CashuPaymentRequestError::MalformedNut18)
        }
    }

    fn as_bool(&self) -> Result<bool, CashuPaymentRequestError> {
        if let Self::Bool(value) = self {
            Ok(*value)
        } else {
            Err(CashuPaymentRequestError::InvalidNut18Field("value"))
        }
    }
}

struct CborDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
    items: usize,
}

impl<'a> CborDecoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            items: 0,
        }
    }

    fn decode(mut self) -> Result<CborValue, CashuPaymentRequestError> {
        let value = self.value(0)?;
        if self.offset != self.bytes.len() {
            return Err(CashuPaymentRequestError::MalformedNut18);
        }
        Ok(value)
    }

    fn value(&mut self, depth: usize) -> Result<CborValue, CashuPaymentRequestError> {
        if depth > MAX_NUT18_DEPTH || self.items >= MAX_NUT18_ITEMS {
            return Err(CashuPaymentRequestError::MalformedNut18);
        }
        self.items += 1;
        let initial = self.byte()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        let len = self.length(additional)?;
        match major {
            0 => Ok(CborValue::Unsigned(len)),
            3 => {
                let bytes = self.take(
                    usize::try_from(len).map_err(|_| CashuPaymentRequestError::MalformedNut18)?,
                )?;
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| CashuPaymentRequestError::MalformedNut18)?;
                Ok(CborValue::Text(text.to_owned()))
            }
            4 => {
                let count = Self::container_len(len)?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.value(depth + 1)?);
                }
                Ok(CborValue::Array(values))
            }
            5 => {
                let count = Self::container_len(len)?;
                let mut values = CborMap::new();
                for _ in 0..count {
                    let key = self.value(depth + 1)?.as_text()?.to_owned();
                    let value = self.value(depth + 1)?;
                    if values.insert(key, value).is_some() {
                        return Err(CashuPaymentRequestError::ConflictingField);
                    }
                }
                Ok(CborValue::Map(values))
            }
            7 if additional == 20 => Ok(CborValue::Bool(false)),
            7 if additional == 21 => Ok(CborValue::Bool(true)),
            7 if additional == 22 => Ok(CborValue::Null),
            _ => Err(CashuPaymentRequestError::MalformedNut18),
        }
    }

    fn length(&mut self, additional: u8) -> Result<u64, CashuPaymentRequestError> {
        match additional {
            value @ 0..=23 => Ok(u64::from(value)),
            24 => Ok(u64::from(self.byte()?)),
            25 => Ok(u64::from(u16::from_be_bytes(self.take_array()?))),
            26 => Ok(u64::from(u32::from_be_bytes(self.take_array()?))),
            27 => Ok(u64::from_be_bytes(self.take_array()?)),
            _ => Err(CashuPaymentRequestError::MalformedNut18),
        }
    }

    fn container_len(value: u64) -> Result<usize, CashuPaymentRequestError> {
        let value = usize::try_from(value).map_err(|_| CashuPaymentRequestError::MalformedNut18)?;
        if value > MAX_NUT18_ITEMS {
            return Err(CashuPaymentRequestError::MalformedNut18);
        }
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, CashuPaymentRequestError> {
        let byte = *self
            .bytes
            .get(self.offset)
            .ok_or(CashuPaymentRequestError::MalformedNut18)?;
        self.offset += 1;
        Ok(byte)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CashuPaymentRequestError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(CashuPaymentRequestError::MalformedNut18)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(CashuPaymentRequestError::MalformedNut18)?;
        self.offset = end;
        Ok(slice)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], CashuPaymentRequestError> {
        self.take(N)?
            .try_into()
            .map_err(|_| CashuPaymentRequestError::MalformedNut18)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    const NUT18_EXAMPLE: &str = "creqApWF0gaNhdGVub3N0cmFheKlucHJvZmlsZTFxeTI4d3VtbjhnaGo3dW45ZDNzaGp0bnl2OWtoMnVld2Q5aHN6OW1od2RlbjV0ZTB3ZmprY2N0ZTljdXJ4dmVuOWVlaHFjdHJ2NWhzenJ0aHdkZW41dGUwZGVoaHh0bnZkYWtxcWd5ZGFxeTdjdXJrNDM5eWtwdGt5c3Y3dWRoZGh1NjhzdWNtMjk1YWtxZWZkZWhrZjBkNDk1Y3d1bmw1YWeBgmFuYjE3YWloYjdhOTAxNzZhYQphdWNzYXRhbYF4Imh0dHBzOi8vbm9mZWVzLnRlc3RudXQuY2FzaHUuc3BhY2U=";

    #[test]
    fn parses_explicit_mint_request_without_contacting_the_mint() {
        let request = CashuPaymentRequest::parse(
            "cashu://request?mint=https%3A%2F%2Fmint.example%2Fapi&amount_sats=100000",
        )
        .unwrap();
        assert_eq!(request.mint_url, "https://mint.example/api/");
        assert_eq!(request.amount, Some(Amount::from_sats(100_000)));
        assert!(request.invoice.is_none());
        assert_eq!(request.encoding, CashuPaymentRequestEncoding::Uri);
    }

    #[test]
    fn parses_the_nut18_reference_creq_request() {
        let request = CashuPaymentRequest::parse(NUT18_EXAMPLE).unwrap();
        assert_eq!(request.encoding, CashuPaymentRequestEncoding::Nut18);
        assert_eq!(request.amount, Some(Amount::from_sats(10)));
        assert_eq!(request.unit.as_deref(), Some("sat"));
        assert_eq!(
            request.mint_urls,
            vec!["https://nofees.testnut.cashu.space/".to_owned()]
        );
        assert_eq!(request.transports[0].kind, "nostr");
    }

    #[test]
    fn malformed_creq_is_a_nut18_validation_error_not_a_token() {
        assert_eq!(
            CashuPaymentRequest::parse("creqA~not-base64").unwrap_err(),
            CashuPaymentRequestError::InvalidNut18Encoding
        );
    }

    #[test]
    fn extracts_an_optional_embedded_invoice_extension() {
        let payload = cbor_map(vec![
            ("a", cbor_uint(100_000)),
            ("u", cbor_text("sat")),
            ("m", cbor_array(vec![cbor_text("https://mint.example")])),
            ("invoice", cbor_text("lnbc1000u1qqqqqqq9kvtew")),
        ]);
        let request =
            CashuPaymentRequest::parse(&format!("creqA{}", URL_SAFE_NO_PAD.encode(payload)))
                .unwrap();
        assert_eq!(
            request.invoice.unwrap().amount(),
            Some(Amount::from_sats(100_000))
        );
    }

    #[test]
    fn does_not_treat_a_cashu_token_as_a_payment_request() {
        assert_eq!(
            CashuPaymentRequest::parse("cashuAeyJ0b2tlbiI6W119").unwrap_err(),
            CashuPaymentRequestError::BearerToken
        );
    }

    fn cbor_map(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut value = cbor_head(5, entries.len() as u64);
        for (key, entry) in entries {
            value.extend(cbor_text(key));
            value.extend(entry);
        }
        value
    }

    fn cbor_array(entries: Vec<Vec<u8>>) -> Vec<u8> {
        let mut value = cbor_head(4, entries.len() as u64);
        for entry in entries {
            value.extend(entry);
        }
        value
    }

    fn cbor_text(value: &str) -> Vec<u8> {
        let mut encoded = cbor_head(3, value.len() as u64);
        encoded.extend(value.as_bytes());
        encoded
    }

    fn cbor_uint(value: u64) -> Vec<u8> {
        cbor_head(0, value)
    }

    fn cbor_head(major: u8, value: u64) -> Vec<u8> {
        let tag = major << 5;
        match value {
            0..=23 => vec![tag | u8::try_from(value).expect("range is bounded")],
            24..=255 => vec![tag | 0x18, u8::try_from(value).expect("range is bounded")],
            256..=65_535 => {
                let mut encoded = vec![tag | 0x19];
                encoded.extend(
                    u16::try_from(value)
                        .expect("range is bounded")
                        .to_be_bytes(),
                );
                encoded
            }
            65_536..=4_294_967_295 => {
                let mut encoded = vec![tag | 0x1a];
                encoded.extend(
                    u32::try_from(value)
                        .expect("range is bounded")
                        .to_be_bytes(),
                );
                encoded
            }
            _ => {
                let mut encoded = vec![tag | 0x1b];
                encoded.extend(value.to_be_bytes());
                encoded
            }
        }
    }
}
