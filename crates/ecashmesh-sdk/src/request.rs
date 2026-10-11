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

#[derive(Deserialize, Clone)]
pub struct DestinationInput {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
    pub mint_url: Option<String>,
}

impl EvaluateRequest {
    pub(crate) fn discovery_hints(
        &self,
        live_destination: Option<&live::LiveDestination>,
    ) -> Result<Vec<ecashmesh_cashu::discovery::MintHint>, Error> {
        use ecashmesh_cashu::discovery::{DiscoverySource, MintHint};
        if self.wallet_mint_urls.len()
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
            .wallet_mint_urls
            .iter()
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
        if self.payment_intent != "send" {
            return Err(Error::validation(
                "payment_intent must be send",
                vec![self.payment_intent.clone()],
            ));
        }
        Ok(())
    }
}
