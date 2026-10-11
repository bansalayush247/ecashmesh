//! The evaluation request: what the wallet asks for, and its validation.

use serde::Deserialize;

use crate::{Error, live, sources::unix_now};

#[derive(Clone, Deserialize)]
pub struct EvaluateRequest {
    pub amount: u64,
    pub asset: String,
    pub destination: DestinationInput,
    pub payment_intent: String,
    #[serde(default)]
    pub candidate_connectors: Vec<String>,
    #[serde(default)]
    pub source_connector: Option<String>,
    #[serde(default)]
    pub source_mint_url: Option<String>,
    #[serde(default)]
    pub wallet_mint_urls: Vec<String>,
    /// Spendable sats the wallet holds at each of its mints, by mint URL.
    /// These mints are wallet mints too. A listed mint whose balance cannot
    /// cover the amount plus its melt fee reserve is excluded.
    #[serde(default)]
    pub wallet_mint_balances: std::collections::BTreeMap<String, u64>,
    /// User-authorized Fedimint connector IDs. These are non-secret labels for
    /// already-configured local adapters; bridge credentials never cross HTTP.
    #[serde(default)]
    pub federation_connector_ids: Vec<String>,
    #[serde(default)]
    pub federation_identities: std::collections::BTreeMap<String, String>,
    /// Explicit empty registry means no authorized sources, never all seeds.
    #[serde(default)]
    pub strict_source_registry: bool,
    #[serde(default)]
    pub mint_urls: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DestinationInput {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
    pub mint_url: Option<String>,
}

impl EvaluateRequest {
    /// The wallet's mints: `wallet_mint_urls` and every mint with a balance.
    pub(crate) fn wallet_mints(&self) -> impl Iterator<Item = &String> {
        self.wallet_mint_urls
            .iter()
            .chain(self.wallet_mint_balances.keys())
    }

    /// Balances by canonical mint URL.
    pub(crate) fn canonical_balances(&self) -> std::collections::BTreeMap<String, u64> {
        self.wallet_mint_balances
            .iter()
            .filter_map(|(url, sats)| {
                ecashmesh_cashu::discovery::canonical_mint_url(url)
                    .ok()
                    .map(|url| (url, *sats))
            })
            .collect()
    }

    pub(crate) fn discovery_hints(
        &self,
        live_destination: Option<&live::LiveDestination>,
    ) -> Result<Vec<ecashmesh_cashu::discovery::MintHint>, Error> {
        use ecashmesh_cashu::discovery::{DiscoverySource, MintHint};
        if self.wallet_mints().count()
            + self.mint_urls.len()
            + usize::from(self.destination.mint_url.is_some())
            + usize::from(self.source_mint_url.is_some())
            + live_destination.map_or(0, |destination| destination.destination_mint_urls().len())
            > 64
        {
            return Err(Error::validation(
                "At most 64 mint URL hints are supported",
                vec!["mint_urls".into()],
            ));
        }
        let hints = self
            .wallet_mints()
            .map(|url| (url.clone(), DiscoverySource::Wallet))
            .chain(
                self.mint_urls
                    .iter()
                    .map(|url| (url.clone(), DiscoverySource::PaymentRequest)),
            )
            .chain(
                self.destination
                    .mint_url
                    .iter()
                    .map(|url| (url.clone(), DiscoverySource::Destination)),
            )
            .chain(
                self.source_mint_url
                    .iter()
                    .map(|url| (url.clone(), DiscoverySource::Wallet)),
            )
            .chain(live_destination.into_iter().flat_map(|destination| {
                destination
                    .destination_mint_urls()
                    .into_iter()
                    .map(|url| (url.to_owned(), DiscoverySource::Destination))
            }))
            .map(|(url, source)| MintHint {
                url,
                alias: None,
                source,
                observed_at: unix_now(),
                stale: false,
            })
            .collect();
        Ok(hints)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.amount == 0 {
            return Err(Error::validation(
                "amount must be greater than zero",
                vec!["amount".into()],
            ));
        }
        if self.asset != "BTC" {
            return Err(Error::validation(
                "asset must be BTC",
                vec![self.asset.clone()],
            ));
        }
        if !matches!(self.destination.kind.as_str(), "lightning" | "cashu")
            || self.destination.value.trim().is_empty()
        {
            return Err(Error::validation(
                "destination must be a non-empty lightning or Cashu target",
                vec!["destination".into()],
            ));
        }
        if let Some(url) = self
            .wallet_mint_balances
            .keys()
            .find(|url| ecashmesh_cashu::discovery::canonical_mint_url(url).is_err())
        {
            return Err(Error::validation(
                "wallet_mint_balances contains an invalid mint URL",
                vec![url.clone()],
            ));
        }
        if self.payment_intent != "send" {
            return Err(Error::validation(
                "payment_intent must be send",
                vec![self.payment_intent.clone()],
            ));
        }
        Ok(())
    }
}
