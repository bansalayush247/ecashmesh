use super::{
    DiscoveryReport, DiscoverySource, MintHint, canonical_mint_url, directory_hints, discover,
};
use crate::{
    CashuAdapter, CashuObservation, EndpointCapture, MintConfig, issue,
    transport::{client_builder, read_url, retain, unix_now},
};
use reqwest::{Client, Url};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::Mutex;

/// A single discovery/evaluation snapshot. No global mint database or wallet state.
pub struct DiscoveryState {
    /// Source claims, canonical identity, and failed discovery diagnostics.
    pub report: DiscoveryReport,
    /// Normalized read-only mint responses.
    pub observations: Vec<CashuObservation>,
}

/// Bounded source configuration and cached metadata reads.
pub struct DiscoveryService {
    seeds: Vec<MintHint>,
    directories: Vec<Url>,
    allowed: BTreeSet<String>,
    ttl: u64,
    client: Client,
    directories_cache: Mutex<BTreeMap<String, EndpointCapture>>,
    adapters: Mutex<BTreeMap<String, Arc<CashuAdapter>>>,
}

impl DiscoveryService {
    /// Creates a discovery service. Seeds/allowlist are operator-controlled local-network exceptions.
    ///
    /// # Errors
    /// Rejects invalid seed/directory/allowlist URLs, conflicting aliases, or oversized configuration.
    pub fn new(
        seeds: Vec<MintHint>,
        directories: Vec<String>,
        allowed_urls: Vec<String>,
        ttl: u64,
    ) -> Result<Self, String> {
        if seeds.len() > 64 || directories.len() > 8 || allowed_urls.len() > 64 || ttl == 0 {
            return Err("Discovery supports up to 64 seeds, 8 directories, 64 allowed URLs, and a positive TTL".into());
        }
        if seeds
            .iter()
            .any(|hint| hint.source != DiscoverySource::Seed)
        {
            return Err("Configured seeds must use the seed source".into());
        }
        let report = discover(seeds.clone(), unix_now(), ttl);
        if report
            .issues
            .iter()
            .any(|issue| issue.code != "STALE_DISCOVERY")
        {
            return Err(format!("Invalid seed configuration: {:?}", report.issues));
        }
        let mut allowed = allowed_urls
            .into_iter()
            .map(|url| canonical_mint_url(&url))
            .collect::<Result<BTreeSet<_>, _>>()?;
        allowed.extend(report.mints.iter().map(|mint| mint.canonical_url.clone()));
        let mut endpoints = Vec::new();
        for endpoint in directories {
            let endpoint = if endpoint == "mint-audit" {
                super::MINT_AUDIT_DIRECTORY.to_owned()
            } else {
                endpoint
            };
            let url = Url::parse(&endpoint).map_err(|_| "Invalid directory URL")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err("Directory URL must be HTTP(S) without credentials or fragment".into());
            }
            endpoints.push(url);
        }
        endpoints.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        endpoints.dedup();
        Ok(Self {
            seeds,
            directories: endpoints,
            allowed,
            ttl,
            client: client_builder()
                .build()
                .map_err(|error| error.to_string())?,
            directories_cache: Mutex::new(BTreeMap::new()),
            adapters: Mutex::new(BTreeMap::new()),
        })
    }

    /// Collects configured and request-scoped discovery sources, then probes canonical mints once.
    /// Untrusted request hints never persist in the registry or expand the allowlist.
    ///
    /// # Errors
    /// Rejects oversized or privileged request hints and unexpected task failures.
    pub async fn collect(&self, hints: Vec<MintHint>) -> Result<DiscoveryState, String> {
        if hints.len() > 64
            || hints.iter().any(|hint| {
                matches!(
                    hint.source,
                    DiscoverySource::Seed | DiscoverySource::Directory(_)
                ) || hint.alias.is_some()
            })
        {
            return Err("At most 64 wallet/payment/destination mint URLs may be submitted".into());
        }
        let (mut hints, mut issues) = self.sources(hints).await;
        hints.extend(self.seeds.clone());
        let mut report = discover(hints, unix_now(), self.ttl);
        report.issues.append(&mut issues);
        let mut adapters = self.adapters.lock().await;
        let mut pending = tokio::task::JoinSet::new();
        for mint in &report.mints {
            let adapter = if let Some(adapter) = adapters.get(&mint.canonical_url) {
                Arc::clone(adapter)
            } else {
                let config = MintConfig::new(mint.connector_id(), &mint.canonical_url, self.ttl)?;
                let config = if self.allowed.contains(&mint.canonical_url) {
                    config
                } else {
                    config.public_only()
                };
                Arc::new(CashuAdapter::new(config).map_err(|error| error.to_string())?)
            };
            // Only configured seed adapters persist, bounding cross-request cache growth.
            if mint
                .provenance
                .iter()
                .any(|hint| hint.source == DiscoverySource::Seed)
            {
                adapters.insert(mint.canonical_url.clone(), Arc::clone(&adapter));
            }
            pending.spawn(async move { adapter.observe().await });
        }
        drop(adapters);
        let mut observations = Vec::new();
        while let Some(result) = pending.join_next().await {
            observations.push(result.map_err(|error| error.to_string())?);
        }
        observations.sort_by(|a, b| a.id.cmp(&b.id));
        report
            .issues
            .sort_by(|a, b| (&a.field, &a.code, &a.message).cmp(&(&b.field, &b.code, &b.message)));
        report.issues.dedup();
        Ok(DiscoveryState {
            report,
            observations,
        })
    }

    async fn sources(&self, mut hints: Vec<MintHint>) -> (Vec<MintHint>, Vec<crate::AdapterIssue>) {
        let mut issues = Vec::new();
        let mut cache = self.directories_cache.lock().await;
        for endpoint in &self.directories {
            let result = read_url(&self.client, endpoint.clone()).await;
            let now = unix_now();
            let capture = retain(result, cache.get(endpoint.as_str()), now);
            let (mut discovered, mut failures) =
                directory_hints(endpoint.as_str(), &capture, unix_now(), self.ttl);
            if discovered.is_empty() {
                issue(
                    &mut failures,
                    endpoint.as_str(),
                    "EMPTY_DIRECTORY",
                    "No usable mint URLs discovered",
                );
            }
            hints.append(&mut discovered);
            issues.append(&mut failures);
            cache.insert(endpoint.to_string(), capture);
        }
        (hints, issues)
    }
}
