# Design decisions and trade-offs

The goal: let value move between independent Cashu mints and Fedimint
federations, and pick the source that can really execute a payment, without
asking anyone to trust EcashMesh more than they already trust their mint.

## 1. Lightning is the common settlement layer

Cashu and Fedimint have no shared protocol, but both already pay and receive
Lightning invoices (Cashu melt / mint quote, Fedimint LNv2 send / receive). A
cross-system payment is therefore an ordinary Lightning payment from the
source's node to an invoice issued by the destination.

- **Gained:** works with unmodified mints and federations today; no new trust
  in EcashMesh, which never holds the funds in transit.
- **Cost:** every transfer pays Lightning routing fees and needs channel
  liquidity on the path. That is why liquidity and the routing-fee budget are
  first-class evidence in the selection layer.
- **Rejected:** a direct mint-to-mint swap protocol. It would need changes to
  both protocols and a new trusted intermediary.

## 2. Native clients, pinned versions

Payments use each system's own client: CDK for Cashu, and Fedimint v0.12.1
clients hosted in one bridge process (`fedimint-bridge`). EcashMesh does not
reimplement either protocol.

- **Gained:** protocol behaviour (fees, gateway selection, refunds) is the real
  behaviour, not our approximation of it.
- **Cost:** heavy first build, pinned versions to keep up to date, and an extra
  bridge process.

## 3. Prove interoperability on a real network, not mocks

The regtest lab runs independent processes (bitcoind, 4 federations with their
gateways, 4 Cashu mints each with its own LND) and real channels. A route
counts only when the Lightning payment settles **and** the destination's
credit is verified (proofs issued or LNv2 receive claimed).

- **Gained:** the 56/56 result is evidence, not a claim. The lab is also the
  end-to-end test.
- **Cost:** the first run compiles Fedimint, CDK and LND from source and takes
  a long time; it needs Nix and a lot of disk.
- **Constraint we kept:** no mocked settlement, synthetic evidence, timeouts
  that hide failures, or special-cased self-payments. A failing route stays a
  failure.

## 4. Real payments only on regtest, and only server-side

Real payments happen only in the lab: the API refuses unless
`PAYMENT_ENVIRONMENT=regtest` and lab mode is on, and listens on loopback only.
The web app asks the API to pay; it never holds ecash, macaroons or passwords.
Live mode (real public mints and federations) is read-only.

- **Gained:** a judge can run real payments safely; no credential reaches the
  browser.
- **Cost:** in the lab the API holds the lab wallets, which is not a production
  custody model, and live mode cannot demonstrate a mainnet payment.
- **Removed:** an earlier in-browser regtest Cashu wallet. It duplicated what
  the lab endpoints do and put proofs in browser storage.

## 5. Feasibility before ranking, from the executor's own numbers

A source is excluded, with a reason, when it cannot fund the payment, cannot
route it, or when the measured route fee exceeds the **routing-fee budget**
of the node that pays it. That budget comes from the same place execution
takes it: the LNv2 gateway's own send parameters (send fee minus minimum send
fee), or the Cashu melt quote's fee reserve. Only feasible sources are ranked.

- **Why:** an earlier version ranked a gateway with a zero routing budget as
  the cheapest option, and its payment was then refused. The fix was in the
  evaluator, not in the display or the executor.
- **Trade-off:** when there is no fresh probe, feasibility is *unknown* and the
  source is ranked normally (with an unknown-evidence penalty) rather than
  excluded on a guess.

## 6. Deterministic scoring, and unknown is never good

Ranking is a fixed, documented weighting of six signals (liquidity,
reliability, fee, freshness, solvency, history) with penalties for unknown,
stale, weak or conflicting evidence. Ties break on fee, hop count, source ID.

- **Gained:** every decision can be explained and reproduced; a quote is not
  treated as proof of liquidity, and a reachable service is not treated as
  reliable.
- **Cost:** the weights are hand-chosen, not learned. A new source scores low
  until it has history (reliability comes only from real recorded payments).

## 7. Make the custodian's trust visible

Ecash is custodial. Where a system exposes it, EcashMesh measures solvency
instead of assuming it: in the lab, each Fedimint guardian's `admin audit` is
read and a federation counts as covered only when a threshold of guardians
agree that assets cover liabilities. Guardians that disagree are flagged as
conflicting. Cashu has no equivalent, so a mint's solvency is reported as
unknown and penalised, never filled in. A wallet balance is never used as a
liability figure.

- **Cost:** guardian audits need guardian API access, so they are lab-only.
  Cryptographic proof-of-reserves / proof-of-liabilities is future work.

## 8. The user owns the source list, on Nostr

Which mints and federations a user trusts is their data. The app can import
mints from their NIP-60 wallet metadata (token events are never read), save
the source list encrypted to their own key (NIP-44, kind 30078) on their
relays, and discover announced mints and federations via NIP-87. Discovered
sources are never enabled automatically, and the API only evaluates sources
the user enabled.

- **Gained:** no EcashMesh account or server-side profile; the list follows the
  user's key.
- **Cost:** relay availability affects sync; without a signer the list stays
  local.
