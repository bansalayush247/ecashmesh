//! Payment-intent primitives that do not depend on a specific connector.
//!
//! Parsing a Lightning invoice here establishes only that the destination is a
//! structurally valid BOLT11 string and, where encoded, its requested amount.
//! It deliberately does not imply that a connector can pay the invoice.

use core::fmt;

use crate::Amount;

/// A structurally validated BOLT11 invoice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightningInvoice {
    raw: String,
    amount: Option<Amount>,
}

impl LightningInvoice {
    /// Parses a checksummed BOLT11 invoice without attempting payment.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a single-case, checksummed BOLT11
    /// invoice, or when its encoded amount cannot be represented in whole sats.
    pub fn parse(value: &str) -> Result<Self, LightningInvoiceError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(LightningInvoiceError::Empty);
        }
        if value.len() > 4_096 || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(LightningInvoiceError::InvalidFormat);
        }
        let has_lower = value.bytes().any(|byte| byte.is_ascii_lowercase());
        let has_upper = value.bytes().any(|byte| byte.is_ascii_uppercase());
        if has_lower && has_upper {
            return Err(LightningInvoiceError::MixedCase);
        }
        let normalized = value.to_ascii_lowercase();
        let separator = normalized
            .rfind('1')
            .filter(|separator| *separator > 0)
            .ok_or(LightningInvoiceError::InvalidFormat)?;
        let (hrp, data) = normalized.split_at(separator);
        let data = &data[1..];
        if data.len() < 7 || !data.bytes().all(is_bech32_character) {
            return Err(LightningInvoiceError::InvalidFormat);
        }
        if bech32_polymod(hrp, data) != 1 {
            return Err(LightningInvoiceError::InvalidChecksum);
        }
        let amount = parse_amount(hrp)?;
        Ok(Self {
            raw: normalized,
            amount,
        })
    }

    /// Returns the normalized, lower-case invoice string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Returns the whole-satoshi amount encoded by the invoice, when present.
    #[must_use]
    pub const fn amount(&self) -> Option<Amount> {
        self.amount
    }
}

/// A BOLT11 invoice parsing failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LightningInvoiceError {
    /// The caller supplied no invoice.
    Empty,
    /// The invoice does not use one case consistently.
    MixedCase,
    /// The HRP or Bech32 payload is not a supported BOLT11 representation.
    InvalidFormat,
    /// The Bech32 checksum is invalid.
    InvalidChecksum,
    /// The invoice represents a fractional-satoshi amount.
    FractionalSatoshiAmount,
    /// The encoded amount overflowed whole satoshis.
    AmountOverflow,
}

impl fmt::Display for LightningInvoiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "Lightning invoice is empty",
            Self::MixedCase => "Lightning invoice must not mix upper and lower case",
            Self::InvalidFormat => "Destination is not a supported BOLT11 invoice",
            Self::InvalidChecksum => "Lightning invoice checksum is invalid",
            Self::FractionalSatoshiAmount => {
                "Lightning invoice amount is not representable in whole satoshis"
            }
            Self::AmountOverflow => "Lightning invoice amount exceeds supported range",
        })
    }
}

impl std::error::Error for LightningInvoiceError {}

fn is_bech32_character(byte: u8) -> bool {
    b"qpzry9x8gf2tvdw0s3jn54khce6mua7l".contains(&byte)
}

fn bech32_value(byte: u8) -> u8 {
    b"qpzry9x8gf2tvdw0s3jn54khce6mua7l"
        .iter()
        .position(|candidate| *candidate == byte)
        .and_then(|index| u8::try_from(index).ok())
        .expect("caller validates Bech32 characters")
}

fn bech32_polymod(hrp: &str, data: &str) -> u32 {
    const GENERATORS: [u32; 5] = [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ];
    let mut checksum = 1_u32;
    for byte in hrp.bytes() {
        checksum = bech32_expand(checksum, byte >> 5, &GENERATORS);
    }
    checksum = bech32_expand(checksum, 0, &GENERATORS);
    for byte in hrp.bytes() {
        checksum = bech32_expand(checksum, byte & 0x1f, &GENERATORS);
    }
    for byte in data.bytes() {
        checksum = bech32_expand(checksum, bech32_value(byte), &GENERATORS);
    }
    checksum
}

fn bech32_expand(checksum: u32, value: u8, generators: &[u32; 5]) -> u32 {
    let top = checksum >> 25;
    let mut next = (checksum & 0x01ff_ffff) << 5 ^ u32::from(value);
    for (index, generator) in generators.iter().enumerate() {
        if (top >> index) & 1 == 1 {
            next ^= generator;
        }
    }
    next
}

fn parse_amount(hrp: &str) -> Result<Option<Amount>, LightningInvoiceError> {
    // `lnbcrt` has `lnbc` as a textual prefix, so match the more specific
    // regtest HRP first.
    let network = ["lnbcrt", "lnbc", "lntb", "lnsb"]
        .iter()
        .find(|prefix| hrp.starts_with(**prefix))
        .ok_or(LightningInvoiceError::InvalidFormat)?;
    let raw = &hrp[network.len()..];
    if raw.is_empty() {
        return Ok(None);
    }
    let (digits, multiplier) = match raw.as_bytes().last().copied() {
        Some(byte @ (b'm' | b'u' | b'n' | b'p')) => (&raw[..raw.len() - 1], Some(byte)),
        _ => (raw, None),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(LightningInvoiceError::InvalidFormat);
    }
    let value = digits
        .parse::<u128>()
        .map_err(|_| LightningInvoiceError::AmountOverflow)?;
    let sats = match multiplier {
        None => value.checked_mul(100_000_000),
        Some(b'm') => value.checked_mul(100_000),
        Some(b'u') => value.checked_mul(100),
        Some(b'n') if value % 10 == 0 => Some(value / 10),
        Some(b'p') if value % 10_000 == 0 => Some(value / 10_000),
        Some(b'n' | b'p') => return Err(LightningInvoiceError::FractionalSatoshiAmount),
        Some(_) => unreachable!("multiplier is validated above"),
    }
    .ok_or(LightningInvoiceError::AmountOverflow)?;
    let sats = u64::try_from(sats).map_err(|_| LightningInvoiceError::AmountOverflow)?;
    Ok(Some(Amount::from_sats(sats)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoice(hrp: &str) -> String {
        // Six zero data values are a valid empty Bech32 checksum payload once
        // the checksum values below are appended. This is enough to exercise
        // destination normalization; signature validation belongs to execution.
        let charset = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut values = vec![0_u8; 7];
        let mut checksum_input = values.clone();
        checksum_input.extend([0_u8; 6]);
        let polymod = bech32_polymod_values(hrp, &checksum_input) ^ 1;
        for shift in (0..6).rev() {
            values.push(u8::try_from((polymod >> (5 * shift)) & 31).unwrap());
        }
        let data = values
            .into_iter()
            .map(|value| char::from(charset[usize::from(value)]))
            .collect::<String>();
        format!("{hrp}1{data}")
    }

    fn bech32_polymod_values(hrp: &str, values: &[u8]) -> u32 {
        const GENERATORS: [u32; 5] = [
            0x3b6a_57b2,
            0x2650_8e6d,
            0x1ea1_19fa,
            0x3d42_33dd,
            0x2a14_62b3,
        ];
        let mut checksum = 1_u32;
        for byte in hrp.bytes() {
            checksum = bech32_expand(checksum, byte >> 5, &GENERATORS);
        }
        checksum = bech32_expand(checksum, 0, &GENERATORS);
        for byte in hrp.bytes() {
            checksum = bech32_expand(checksum, byte & 0x1f, &GENERATORS);
        }
        for value in values {
            checksum = bech32_expand(checksum, *value, &GENERATORS);
        }
        checksum
    }

    #[test]
    fn parses_normalized_whole_sat_invoice_amounts() {
        let value = invoice("lnbc1000u");
        let parsed = LightningInvoice::parse(&value.to_ascii_uppercase()).unwrap();
        assert_eq!(parsed.amount(), Some(Amount::from_sats(100_000)));
        assert_eq!(parsed.as_str(), value);
    }

    #[test]
    fn parses_core_lightning_regtest_invoice() {
        let invoice = "lnbcrt10u1p4255nrsp5q0j39t60cz5gkcttr0kuxd3z0784vyccgfhvrc7menf7sx5lwzpqpp5pqdlzczsy8swldt7yk27962dq6hgdhw2ktwmu3qxdkvfqwtlg5dsdqlg43kzumgf4jhx6pqg9gyjgrrdpjkx6cxqyjw5qcqp2rzjq2xklnvv0s8vzl0nym8v3fzxwas00d2x3xat6ye64dlt7ynql4du5qqq7yqqqqgqqqqqqqlgqqqqqqgq2q9qxpqysgq809aah09kj20ugtpxcldd578r9ycq48t86l9pmz6aer6szwf36uq82ayuhdwsew3pdx9n9pckmjcycjv2ncczheh95casn4eux337uqpx63st6";
        let parsed = LightningInvoice::parse(invoice).expect("valid Core Lightning invoice");
        assert_eq!(parsed.amount(), Some(Amount::from_sats(1_000)));
    }

    #[test]
    fn rejects_invalid_or_fractional_invoices() {
        assert_eq!(
            LightningInvoice::parse("").unwrap_err(),
            LightningInvoiceError::Empty
        );
        assert_eq!(
            LightningInvoice::parse("lnbc1not-a-checksum").unwrap_err(),
            LightningInvoiceError::InvalidFormat
        );
        let fractional = invoice("lnbc1n");
        assert_eq!(
            LightningInvoice::parse(&fractional).unwrap_err(),
            LightningInvoiceError::FractionalSatoshiAmount
        );
    }
}
