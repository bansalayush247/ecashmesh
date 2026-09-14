use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ecashmesh_core::ConnectorId;
use reqwest::{Client, Url, redirect::Policy};
use tokio::sync::Mutex;

use crate::{CashuObservation, EndpointCapture, MintCapture};

const MAX_BODY_BYTES: usize = 1_048_576;

/// Operator-configured mint. Request bodies cannot select arbitrary mint URLs.
#[derive(Clone, Debug)]
pub struct MintConfig {
    id: ConnectorId,
    url: Url,
    max_age_seconds: u64,
    public_only: bool,
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

/// Only performs public GETs (/v1/info, /v1/keysets, /v1/keys), with bounded reads and no redirects.
/// Failed refreshes retain previous bodies as stale and expose the current failure.
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

    async fn fetch(&self, path: &str) -> Result<(String, u64), String> {
        let url = self
            .config
            .url
            .join(path)
            .map_err(|_| "Invalid endpoint URL")?;
        if self.config.public_only {
            let client = public_client(&url).await?;
            read_url(&client, url).await
        } else {
            read_url(&self.client, url).await
        }
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
    let mut response = client.get(url).send().await.map_err(|error| {
        if error.is_timeout() {
            "HTTP request timed out"
        } else {
            "HTTP connection failed"
        }
        .to_owned()
    })?;
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
