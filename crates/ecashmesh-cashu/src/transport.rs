use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ecashmesh_core::{
    Amount, ConfidenceLevel, ConnectorId, Evidence, EvidenceSource, EvidenceTimestamp,
    LightningInvoice,
};
use reqwest::{Client, Url, redirect::Policy};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{AdapterIssue, CashuObservation, EndpointCapture, MintCapture};

const MAX_BODY_BYTES: usize = 1_048_576;

/// Operator-configured mint. Request bodies cannot select arbitrary mint URLs.
#[derive(Clone, Debug)]
pub struct MintConfig {
    id: ConnectorId,
    url: Url,
    max_age_seconds: u64,
    public_only: bool,
}

/// An unpaid NUT-04 Lightning mint quote from a destination mint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MintQuote {
    /// Opaque quote identifier returned by the mint.
    pub quote_id: String,
    /// Destination invoice created by the mint.
    pub invoice: LightningInvoice,
    /// Mint-reported expiry, when supplied.
    pub expires_at_unix_seconds: Option<u64>,
}

/// An unpaid NUT-05 Lightning melt quote from a source mint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeltQuote {
    /// Opaque quote identifier returned by the mint.
    pub quote_id: String,
    /// Upper-bound fee reserve reported for this exact invoice. It is not a
    /// guaranteed final Lightning fee; a later NUT-08 settlement could differ.
    pub fee_reserve_sats: Amount,
    /// Mint-reported expiry, when supplied.
    pub expires_at_unix_seconds: Option<u64>,
}

/// Result returned by the NUT-08 melt endpoint. `final_fee_sats` is deliberately
/// optional: a mint can report a successful settlement without supplying enough
/// information to calculate the final Lightning fee.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeltExecution {
    pub state: String,
    pub payment_preimage: Option<String>,
    pub final_fee_sats: Option<Amount>,
    pub change: Vec<Value>,
    pub raw: Value,
}

/// Write-only NUT-08 client. It owns no secrets and never synthesizes proofs;
/// a wallet custody adapter must supply selected, valid protocol proofs and
/// blinded change outputs. This makes the HTTP call a real Cashu melt, rather
/// than a simulator or a quote being misrepresented as settlement.
pub struct CashuMeltExecutor {
    url: Url,
    client: Client,
}

impl CashuMeltExecutor {
    /// Creates an execution client for an operator-approved mint URL.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid mint URL or HTTP client configuration.
    pub fn new(mint_url: &str) -> Result<Self, String> {
        let config = MintConfig::new(
            ConnectorId::new("cashu:execution").map_err(|error| error.to_string())?,
            mint_url,
            300,
        )?;
        Ok(Self {
            url: config.url,
            client: client_builder()
                .build()
                .map_err(|error| error.to_string())?,
        })
    }

    /// Submits actual NUT-07/NUT-08 input proofs to an already-created melt
    /// quote. Inputs and outputs are protocol JSON intentionally kept opaque to
    /// avoid an incompatible home-grown Cashu cryptography implementation.
    ///
    /// # Errors
    ///
    /// Returns protocol, transport, or malformed-response failures without retrying.
    pub async fn melt(
        &self,
        quote: &str,
        inputs: Vec<Value>,
        outputs: Vec<Value>,
    ) -> Result<MeltExecution, String> {
        if quote.trim().is_empty() || quote.len() > 4_096 || inputs.is_empty() {
            return Err("Melt requires a quote and at least one proof".into());
        }
        let url = self
            .url
            .join("v1/melt/bolt11")
            .map_err(|_| "Invalid melt endpoint")?;
        let (body, _) = post_url(
            &self.client,
            url,
            json!({
                "quote": quote,
                "inputs": inputs,
                "outputs": outputs,
            }),
        )
        .await?;
        parse_melt_execution(&body)
    }

    /// Reads the canonical quote state for recovery after a timeout or pending
    /// response. No payment is retried by this method.
    ///
    /// # Errors
    ///
    /// Returns a transport or malformed-response failure.
    pub async fn status(&self, quote: &str) -> Result<MeltExecution, String> {
        let url = self
            .url
            .join(&format!("v1/melt/quote/bolt11/{quote}"))
            .map_err(|_| "Invalid melt status endpoint")?;
        let (body, _) = read_url(&self.client, url).await?;
        parse_melt_execution(&body)
    }
}

/// A quote observation with explicit missing/malformed/unavailable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuoteObservation<T> {
    /// Current quote data, or unknown when it could not be safely normalized.
    pub evidence: Evidence<T>,
    /// Exact endpoint queried.
    pub endpoint: String,
    /// Inspectable reason a quote is unavailable or unusable.
    pub issue: Option<AdapterIssue>,
}

impl MintConfig {
    /// Validates an HTTP(S) mint base URL (including optional deployment subpath).
    ///
    /// # Errors
    /// Rejects non-HTTP URLs, credentials, query strings, fragments, or a zero TTL.
    pub fn new(id: ConnectorId, url: &str, max_age_seconds: u64) -> Result<Self, String> {
        let mut url = Url::parse(&crate::discovery::canonical_mint_url(url)?)
            .map_err(|error| error.to_string())?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || max_age_seconds == 0
        {
            return Err("Mint URL must be HTTP(S), without credentials/query/fragment; TTL must be positive".into());
        }
        url.set_path(&format!("{}/", url.path().trim_end_matches('/')));
        Ok(Self {
            id,
            url,
            max_age_seconds,
            public_only: false,
        })
    }

    /// Restricts an untrusted discovery target to public addresses with pinned DNS.
    #[must_use]
    pub fn public_only(mut self) -> Self {
        self.public_only = true;
        self
    }
}

/// Reads public metadata and requests unpaid NUT-04/NUT-05 quotes with bounded I/O and no redirects.
/// It never invokes payment, melt, or token-management endpoints. Failed refreshes retain previous
/// bodies as stale and expose the current failure.
pub struct CashuAdapter {
    config: MintConfig,
    client: Client,
    cache: Mutex<Option<MintCapture>>,
}

impl CashuAdapter {
    /// Creates a read-only client without contacting a mint.
    ///
    /// # Errors
    /// Returns an error if the TLS/HTTP client cannot be initialized.
    pub fn new(config: MintConfig) -> Result<Self, reqwest::Error> {
        Ok(Self {
            config,
            client: client_builder().build()?,
            cache: Mutex::new(None),
        })
    }

    /// Fetches public metadata; transport failures become evidence rather than panics.
    pub async fn observe(&self) -> CashuObservation {
        let mut cache = self.cache.lock().await;
        let (info, keysets, keys) = tokio::join!(
            self.fetch("v1/info"),
            self.fetch("v1/keysets"),
            self.fetch("v1/keys")
        );
        let now = unix_now();
        let capture = MintCapture {
            info: retain(info, cache.as_ref().map(|old| &old.info), now),
            keysets: retain(keysets, cache.as_ref().map(|old| &old.keysets), now),
            keys: Some(retain(
                keys,
                cache.as_ref().and_then(|old| old.keys.as_ref()),
                now,
            )),
        };
        let observation = capture.normalize(
            self.config.id.clone(),
            self.config.url.as_str(),
            now,
            self.config.max_age_seconds,
        );
        *cache = Some(capture);
        observation
    }

    /// Requests an unpaid NUT-04 Lightning mint quote for `amount`.
    ///
    /// This endpoint is used only to discover a real destination invoice. It
    /// does not send value, redeem tokens, or handle custody material.
    #[must_use]
    pub async fn mint_quote(&self, amount: Amount) -> QuoteObservation<MintQuote> {
        let path = "v1/mint/quote/bolt11";
        let endpoint = self.endpoint(path);
        match self
            .post(path, json!({ "amount": amount.sats(), "unit": "sat" }))
            .await
            .and_then(|(body, observed_at)| parse_mint_quote(&body, amount, observed_at))
        {
            Ok((quote, observed_at)) => QuoteObservation {
                evidence: Evidence::reported(
                    quote,
                    EvidenceSource::Connector,
                    EvidenceTimestamp::from_unix_seconds(observed_at),
                    ConfidenceLevel::Medium,
                ),
                endpoint,
                issue: None,
            },
            Err(error) => QuoteObservation {
                evidence: Evidence::Unknown,
                endpoint,
                issue: Some(AdapterIssue {
                    field: "mint_quote".into(),
                    code: quote_error_code(&error).into(),
                    message: error,
                }),
            },
        }
    }

    /// Requests an unpaid NUT-05 Lightning melt quote for one exact invoice.
    ///
    /// The adapter never invokes the melt execution endpoint, so this cannot
    /// move funds or access proofs, tokens, or private keys.
    #[must_use]
    pub async fn melt_quote(
        &self,
        invoice: &LightningInvoice,
        amount: Amount,
    ) -> QuoteObservation<MeltQuote> {
        let path = "v1/melt/quote/bolt11";
        let endpoint = self.endpoint(path);
        match self
            .post(path, json!({ "request": invoice.as_str(), "unit": "sat" }))
            .await
            .and_then(|(body, observed_at)| parse_melt_quote(&body, amount, observed_at))
        {
            Ok((quote, observed_at)) => QuoteObservation {
                evidence: Evidence::reported(
                    quote,
                    EvidenceSource::Connector,
                    EvidenceTimestamp::from_unix_seconds(observed_at),
                    ConfidenceLevel::Medium,
                ),
                endpoint,
                issue: None,
            },
            Err(error) => QuoteObservation {
                evidence: Evidence::Unknown,
                endpoint,
                issue: Some(AdapterIssue {
                    field: "melt_quote".into(),
                    code: quote_error_code(&error).into(),
                    message: error,
                }),
            },
        }
    }

    async fn fetch(&self, path: &str) -> Result<(String, u64), String> {
        let url = self.url(path)?;
        if self.config.public_only {
            let client = public_client(&url).await?;
            read_url(&client, url).await
        } else {
            read_url(&self.client, url).await
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<(String, u64), String> {
        let url = self.url(path)?;
        if self.config.public_only {
            let client = public_client(&url).await?;
            post_url(&client, url, body).await
        } else {
            post_url(&self.client, url, body).await
        }
    }

    fn endpoint(&self, path: &str) -> String {
        self.config
            .url
            .join(path)
            .map_or_else(|_| self.config.url.to_string(), |url| url.to_string())
    }

    fn url(&self, path: &str) -> Result<Url, String> {
        self.config
            .url
            .join(path)
            .map_err(|_| "Invalid endpoint URL".into())
    }
}

pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .redirect(Policy::none())
        .no_proxy()
        .user_agent("ecashmesh-cashu/0.1 read-only")
}

async fn public_client(url: &Url) -> Result<Client, String> {
    let host = url
        .host_str()
        .ok_or("Missing host")?
        .trim_matches(['[', ']']);
    let port = url.port_or_known_default().ok_or("Missing port")?;
    let addresses = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| "DNS lookup timed out")?
    .map_err(|_| "DNS lookup failed")?
    .take(17)
    .collect::<Vec<_>>();
    if addresses.is_empty()
        || addresses.len() > 16
        || addresses.iter().any(|address| !public_ip(address.ip()))
    {
        return Err("Discovery target resolves to a non-public or unsupported address".into());
    }
    client_builder()
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|error| error.to_string())
}

pub(crate) fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        std::net::IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Conservative global unicast policy; exclude transition/documentation space.
            segments[0] & 0xe000 == 0x2000
                && segments[0] != 0x2001
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}

pub(crate) async fn read_url(client: &Client, url: Url) -> Result<(String, u64), String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| request_error(&error))?;
    read_response(response).await
}

async fn post_url(client: &Client, url: Url, body: Value) -> Result<(String, u64), String> {
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|error| request_error(&error))?;
    read_response(response).await
}

fn request_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "HTTP request timed out"
    } else {
        "HTTP connection failed"
    }
    .to_owned()
}

async fn read_response(response: reqwest::Response) -> Result<(String, u64), String> {
    let mut response = response;
    if !response.status().is_success() {
        return Err(format!("HTTP status {}", response.status().as_u16()));
    }
    let age = response
        .headers()
        .get("age")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|text| text.parse::<u64>().ok())
                .ok_or_else(|| "Malformed HTTP Age header".to_owned())
        })
        .transpose()?
        .unwrap_or(0);
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err("Response exceeds size limit".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Failed reading response")?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err("Response exceeds size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    // Timestamp this response independently; a slower sibling request must
    // not make an earlier response look newer.
    let observed_at = unix_now().saturating_sub(age);
    String::from_utf8(body)
        .map(|body| (body, observed_at))
        .map_err(|_| "Response is not UTF-8".into())
}

fn parse_mint_quote(
    body: &str,
    expected_amount: Amount,
    observed_at: u64,
) -> Result<(MintQuote, u64), String> {
    let value = crate::strict_json::parse(body).map_err(|_| "Malformed mint quote JSON")?;
    let quote_id = quote_string(&value, "quote")?;
    let invoice = quote_string(&value, "request")
        .and_then(|invoice| LightningInvoice::parse(&invoice).map_err(|error| error.to_string()))?;
    if invoice.amount() != Some(expected_amount) {
        return Err("Mint quote invoice amount does not match payment amount".into());
    }
    Ok((
        MintQuote {
            quote_id,
            invoice,
            expires_at_unix_seconds: quote_expiry(&value),
        },
        observed_at,
    ))
}

fn parse_melt_quote(
    body: &str,
    expected_amount: Amount,
    observed_at: u64,
) -> Result<(MeltQuote, u64), String> {
    let value = crate::strict_json::parse(body).map_err(|_| "Malformed melt quote JSON")?;
    let quote_id = quote_string(&value, "quote")?;
    if quote_u64(&value, "amount")? != expected_amount.sats() {
        return Err("Melt quote amount does not match payment amount".into());
    }
    let fee = value
        .get("fee_reserve")
        .or_else(|| value.get("fee"))
        .and_then(Value::as_u64)
        .ok_or_else(|| "Melt quote did not include a whole-sat fee reserve".to_owned())?;
    Ok((
        MeltQuote {
            quote_id,
            fee_reserve_sats: Amount::from_sats(fee),
            expires_at_unix_seconds: quote_expiry(&value),
        },
        observed_at,
    ))
}

fn parse_melt_execution(body: &str) -> Result<MeltExecution, String> {
    let value = crate::strict_json::parse(body).map_err(|_| "Malformed melt execution JSON")?;
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("paid")
                .and_then(Value::as_bool)
                .map(|paid| if paid { "PAID" } else { "UNPAID" })
        })
        .map(str::to_ascii_uppercase)
        .ok_or_else(|| "Melt response did not include a state".to_owned())?;
    if !matches!(state.as_str(), "PAID" | "PENDING" | "UNPAID" | "FAILED") {
        return Err("Melt response has an unsupported state".into());
    }
    let final_fee_sats = value
        .get("fee_paid")
        .or_else(|| value.get("fee"))
        .and_then(Value::as_u64)
        .map(Amount::from_sats);
    let change = value
        .get("change")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(MeltExecution {
        state,
        payment_preimage: value
            .get("payment_preimage")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        final_fee_sats,
        change,
        raw: value,
    })
}

fn quote_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 4_096)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("Quote has no valid {field}"))
}

fn quote_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Quote has no valid whole-sat {field}"))
}

fn quote_expiry(value: &Value) -> Option<u64> {
    value.get("expiry").and_then(Value::as_u64)
}

fn quote_error_code(error: &str) -> &'static str {
    if error.starts_with("HTTP") {
        "UNAVAILABLE"
    } else if error.starts_with("Malformed") || error.starts_with("Quote has") {
        "MALFORMED_DATA"
    } else {
        "UNUSABLE_QUOTE"
    }
}

pub(crate) fn retain(
    result: Result<(String, u64), String>,
    old: Option<&EndpointCapture>,
    now: u64,
) -> EndpointCapture {
    match result {
        Ok((body, observed_at)) => EndpointCapture {
            checked_at: now,
            observed_at: Some(observed_at),
            body: Some(body),
            error: None,
            stale: false,
        },
        Err(error) => EndpointCapture {
            checked_at: now,
            observed_at: old.and_then(|old| old.observed_at),
            body: old.and_then(|old| old.body.clone()),
            error: Some(error),
            stale: true,
        },
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
