//! `EcashMesh` SDK: choose which of a wallet's ecash sources should pay a
//! Lightning invoice or Cashu payment request.
//!
//! A wallet configures its Cashu mints and Fedimint federations once, then
//! asks [`EcashMesh::evaluate`] for each payment. The SDK reads quotes from
//! each source, excludes sources that cannot fund or route the payment (with
//! a reason), and ranks the rest with an explanation. It never moves funds.
//!
//! ```no_run
//! # async fn run(cashu: ecashmesh_cashu::discovery::DiscoveryService,
//! #              fedimint: ecashmesh_fedimint::FedimintService,
//! #              request: ecashmesh_sdk::EvaluateRequest) -> Result<(), ecashmesh_sdk::Error> {
//! let mesh = ecashmesh_sdk::EcashMesh::builder(cashu, fedimint).build();
//! let decision = mesh.evaluate(request).await?;
//! println!("pay with {}", decision.recommended_source.source_id);
//! # Ok(()) }
//! ```
//!
//! Optional evidence comes from the host through two traits:
//! [`LiquidityOracle`] (can this source's Lightning node route the amount?)
//! and [`PaymentHistory`] (how did real payments through it go?). Without
//! them that evidence is unknown and penalised, never assumed.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use ecashmesh_cashu::discovery::DiscoveryService;
use ecashmesh_core::Amount;
use ecashmesh_fedimint::FedimintService;
use futures::future::BoxFuture;
use serde_json::Value;

mod comparison;
mod error;
mod evaluate;
mod evidence_items;
mod fee_budget;
pub mod liquidity;
mod live;
pub mod outcomes;
pub mod reliability;
mod request;
mod response;
mod sources;

pub use error::{Error, ErrorKind};
pub use outcomes::OutcomeLog;
pub use reliability::PaymentRecord;
pub use request::EvaluateRequest;
pub use response::{EvaluateResponse, EvaluatedRouteResponse, deterministic_id};

/// Amount-specific Lightning liquidity evidence for a source, such as probes
/// or channel balances read from the source's own node.
pub trait LiquidityOracle: Send + Sync {
    /// Whether evidence can be read for this source at all.
    fn covers(&self, source_id: &str) -> bool;

    /// Evidence for paying `invoice` (to `payee`, when decodable) from this
    /// source. `None` means nothing is known.
    fn read<'a>(
        &'a self,
        source_id: &'a str,
        invoice: &'a str,
        payee: Option<&'a str>,
        amount: Amount,
        now: u64,
    ) -> BoxFuture<'a, Option<liquidity::SourceLiquidity>>;
}

/// Real payment outcomes per source: the only input to reliability.
pub trait PaymentHistory: Send + Sync {
    /// Reliability for each source; sources without history are unknown.
    fn reliability(
        &self,
        sources: &[&str],
        now: u64,
    ) -> BTreeMap<String, reliability::ReliabilityStats>;

    /// Stores what one evaluation observed per source. Failures to persist
    /// must not fail the evaluation.
    fn record<'a>(&'a self, observations: &'a [Value]) -> BoxFuture<'a, ()>;
}

/// Default end-to-end deadline for one evaluation, including discovery,
/// destination quoting and every source read.
pub const DEFAULT_EVALUATION_DEADLINE: Duration = Duration::from_secs(12);

/// Configures an [`EcashMesh`].
pub struct Builder {
    cashu: DiscoveryService,
    fedimint: FedimintService,
    allow_discovered_sources: bool,
    liquidity: Option<Arc<dyn LiquidityOracle>>,
    history: Option<Arc<dyn PaymentHistory>>,
    deadline: Duration,
}

impl Builder {
    /// Lets sources found through discovery (directories, payment requests)
    /// pay, not only the wallet's own. Off by default.
    #[must_use]
    pub const fn allow_discovered_sources(mut self, allow: bool) -> Self {
        self.allow_discovered_sources = allow;
        self
    }

    #[must_use]
    pub fn liquidity_oracle(mut self, oracle: Arc<dyn LiquidityOracle>) -> Self {
        self.liquidity = Some(oracle);
        self
    }

    #[must_use]
    pub fn payment_history(mut self, history: Arc<dyn PaymentHistory>) -> Self {
        self.history = Some(history);
        self
    }

    /// Uses the wallet's own payment outcomes as both liquidity and
    /// reliability evidence: the evidence available on live networks.
    #[must_use]
    pub fn outcome_log(mut self, log: Arc<OutcomeLog>) -> Self {
        self.liquidity = Some(Arc::clone(&log) as Arc<dyn LiquidityOracle>);
        self.history = Some(log);
        self
    }

    #[must_use]
    pub const fn evaluation_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    #[must_use]
    pub fn build(self) -> EcashMesh {
        EcashMesh {
            sources: sources::Sources::new(
                Arc::new(self.cashu),
                Arc::new(self.fedimint),
                self.allow_discovered_sources,
                self.liquidity,
                self.history,
            ),
            deadline: self.deadline,
        }
    }
}

/// The source-selection engine. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct EcashMesh {
    sources: sources::Sources,
    deadline: Duration,
}

impl EcashMesh {
    #[must_use]
    pub const fn builder(cashu: DiscoveryService, fedimint: FedimintService) -> Builder {
        Builder {
            cashu,
            fedimint,
            allow_discovered_sources: false,
            liquidity: None,
            history: None,
            deadline: DEFAULT_EVALUATION_DEADLINE,
        }
    }

    /// Recommends a source for the payment, with alternatives, exclusions and
    /// the evidence behind each.
    ///
    /// # Errors
    /// `InvalidRequest` for a bad request, `NoViableRoute` when every source
    /// was excluded (diagnostics say why), `Deadline` when it ran out of time.
    pub async fn evaluate(&self, request: EvaluateRequest) -> Result<EvaluateResponse, Error> {
        tokio::time::timeout(self.deadline, evaluate::evaluate(&self.sources, request))
            .await
            .map_err(|_| {
                Error::new(
                    ErrorKind::Deadline,
                    "EVALUATION_DEADLINE",
                    "Source discovery exceeded the evaluation deadline. Refresh sources and retry.",
                )
            })?
    }

    /// Orders the current fee quotes only, ignoring balances. Nothing in it is
    /// executable.
    ///
    /// # Errors
    /// As [`Self::evaluate`], except that having no viable route is not one.
    pub async fn compare(&self, request: EvaluateRequest) -> Result<Value, Error> {
        comparison::compare(self.evaluate(request).await)
    }

    /// Observes the request's sources and discovery hints without quoting.
    ///
    /// # Errors
    /// `InvalidRequest` for a bad request or failed mint discovery.
    pub async fn discover(&self, request: EvaluateRequest) -> Result<Value, Error> {
        request.validate()?;
        let destination =
            live::LiveDestination::parse(&request.destination.kind, &request.destination.value)
                .ok();
        let hints = request.discovery_hints(destination.as_ref())?;
        let batch = self
            .sources
            .collect(Amount::from_sats(request.amount), hints)
            .await?;
        Ok(evaluate::catalog_json(&batch, request.amount))
    }

    /// Every configured source with its current evidence for `amount` sats.
    ///
    /// # Errors
    /// `InvalidRequest` for a zero amount or failed mint discovery.
    pub async fn observations(&self, amount: u64) -> Result<Value, Error> {
        if amount == 0 {
            return Err(Error::validation(
                "amount must be greater than zero",
                vec!["amount".into()],
            ));
        }
        let batch = self
            .sources
            .collect(Amount::from_sats(amount), Vec::new())
            .await?;
        Ok(evaluate::catalog_json(&batch, amount))
    }

    /// The Fedimint adapter, for hosts that manage federations (joining one
    /// from an invite).
    #[must_use]
    pub const fn fedimint(&self) -> &Arc<FedimintService> {
        self.sources.fedimint_service()
    }

    /// The host's payment history, if one was configured.
    #[must_use]
    pub fn payment_history(&self) -> Option<&dyn PaymentHistory> {
        self.sources.history()
    }
}
