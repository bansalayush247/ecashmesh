import type { TransferPlan } from "@ecashmesh/client/http";
import { useEffect, useState } from "react";

import { readToken, type TokenInfo } from "./cashu";
import {
  apiUrl,
  checkMint,
  describe,
  type FediPreview,
  issue,
  type MintCheck,
  moveToCashu,
  previewToFedi,
  rankMints,
  type RankedMint,
} from "./flows";
import {
  forgetToken,
  pendingIssue,
  type PendingIssue,
  recentMints,
  rememberMint,
  savedTokens,
  type SavedToken,
} from "./store";
import {
  injectedWallet,
  labSources,
  labWallet,
  type LabSources,
  type LightningWallet,
} from "./webln";

type Direction = "to-cashu" | "to-fedi";

export function App() {
  const [wallet, setWallet] = useState<LightningWallet | null>(null);
  const [lab, setLab] = useState<LabSources>({ federations: [], mints: [] });
  const [connecting, setConnecting] = useState(true);
  const [direction, setDirection] = useState<Direction>("to-cashu");
  const [tokens, setTokens] = useState<SavedToken[]>(savedTokens);
  const [pending, setPending] = useState<PendingIssue | null>(pendingIssue);

  useEffect(() => {
    void (async () => {
      const injected = await injectedWallet().catch(() => null);
      const sources = await labSources(apiUrl);
      setLab(sources);
      const first = sources.federations[0];
      setWallet(injected ?? (first ? labWallet(apiUrl, first) : null));
      setConnecting(false);
    })();
  }, []);

  const refresh = () => {
    setTokens(savedTokens());
    setPending(pendingIssue());
  };

  return (
    <main className="app">
      <header className="masthead">
        <h1>EcashMesh</h1>
        <p>Move sats between Fedi and Cashu mints, with the fee shown first.</p>
      </header>

      <WalletBar
        wallet={wallet}
        connecting={connecting}
        lab={lab}
        onChoose={(id) => {
          const federation = lab.federations.find((item) => item.id === id);
          if (federation) setWallet(labWallet(apiUrl, federation));
        }}
      />

      {pending && wallet && (
        <PendingBanner pending={pending} onDone={refresh} />
      )}

      {wallet && (
        <>
          <div className="tabs" role="tablist">
            <button
              role="tab"
              aria-selected={direction === "to-cashu"}
              onClick={() => setDirection("to-cashu")}
            >
              Fedi → Cashu
            </button>
            <button
              role="tab"
              aria-selected={direction === "to-fedi"}
              onClick={() => setDirection("to-fedi")}
            >
              Cashu → Fedi
            </button>
          </div>
          {direction === "to-cashu" ? (
            <ToCashu
              wallet={wallet}
              suggestions={lab.mints}
              onToken={refresh}
            />
          ) : (
            <ToFedi wallet={wallet} onDone={refresh} />
          )}
        </>
      )}

      {tokens.length > 0 && <Tokens tokens={tokens} onForget={refresh} />}
    </main>
  );
}

function WalletBar(props: {
  wallet: LightningWallet | null;
  connecting: boolean;
  lab: LabSources;
  onChoose(id: string): void;
}) {
  if (props.connecting) {
    return <section className="wallet">Connecting to your wallet…</section>;
  }
  if (!props.wallet) {
    return (
      <section className="wallet missing">
        <strong>No Lightning wallet found.</strong> Open this mini app inside
        Fedi, which gives it access to your federation through WebLN.
      </section>
    );
  }
  const labMode = props.wallet.kind === "lab";
  return (
    <section className="wallet">
      <span className="label">Lightning wallet</span>
      {labMode ? (
        <select
          aria-label="Lab federation"
          onChange={(event) => props.onChoose(event.target.value)}
        >
          {props.lab.federations.map((federation) => (
            <option key={federation.id} value={federation.id}>
              {federation.label} (lab stand-in for Fedi)
            </option>
          ))}
        </select>
      ) : (
        <strong>{props.wallet.label}</strong>
      )}
    </section>
  );
}

function ToCashu(props: {
  wallet: LightningWallet;
  suggestions: { url: string; label: string }[];
  onToken(): void;
}) {
  const [candidates, setCandidates] = useState<string[]>(() => [
    ...recentMints(),
    ...props.suggestions.map((mint) => mint.url),
  ]);
  const [newMint, setNewMint] = useState("");
  const [amount, setAmount] = useState("1000");
  const [ranked, setRanked] = useState<RankedMint[] | null>(null);
  const [chosen, setChosen] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{
    token: string;
    amount: number;
    mint: string;
  }>();
  const sats = Number.parseInt(amount, 10);
  const valid = candidates.length > 0 && Number.isSafeInteger(sats) && sats > 0;
  const label = (url: string) =>
    props.suggestions.find((mint) => sameHost(mint.url, url))?.label ??
    new URL(url).host;
  const pick = ranked?.find((mint) => mint.healthy && mint.canReceive);
  const selected = ranked?.find((mint) => mint.url === chosen);

  const reset = () => {
    setRanked(null);
    setChosen(null);
    setError(null);
    setResult(undefined);
  };

  async function run(work: () => Promise<void>) {
    setError(null);
    try {
      await work();
    } catch (failure) {
      setError(describe(failure));
    } finally {
      setBusy(null);
    }
  }

  if (result) {
    return (
      <section className="panel">
        <h2>Your Cashu token</h2>
        <p>
          {result.amount} sats of ecash at {label(result.mint)}. Copy it into
          any Cashu wallet; until you do, this token is the money.
        </p>
        <TokenBox token={result.token} />
        <button className="secondary" onClick={reset}>
          Move more
        </button>
      </section>
    );
  }

  return (
    <section className="panel">
      <h2>Get Cashu ecash, paid from Fedi</h2>
      <label>
        Amount (sats)
        <input
          value={amount}
          inputMode="numeric"
          onChange={(event) => {
            setAmount(event.target.value.replace(/\D/g, ""));
            reset();
          }}
        />
      </label>

      <div className="add-mint">
        <label>
          Add a mint
          <input
            value={newMint}
            placeholder="https://mint.example"
            inputMode="url"
            onChange={(event) => setNewMint(event.target.value)}
          />
        </label>
        <button
          className="secondary"
          disabled={!/^https?:\/\/\S+$/.test(newMint.trim())}
          onClick={() => {
            const url = newMint.trim();
            rememberMint(url);
            setCandidates((list) => [url, ...list.filter((u) => u !== url)]);
            setNewMint("");
            reset();
          }}
        >
          Add
        </button>
      </div>

      {ranked ? (
        <div className="ranking" role="radiogroup" aria-label="Mints">
          <p className="note">
            EcashMesh&apos;s order: online and able to receive first, then the
            lowest fee for spending the ecash later.
          </p>
          {ranked.map((mint, index) => {
            const usable = mint.healthy && mint.canReceive;
            return (
              <button
                key={mint.url}
                role="radio"
                aria-checked={chosen === mint.url}
                disabled={!usable}
                className={`rank-row${chosen === mint.url ? " chosen" : ""}`}
                onClick={() => setChosen(mint.url)}
              >
                <span className="rank">{usable ? index + 1 : "–"}</span>
                <span className="rank-body">
                  <span className="rank-title">
                    {label(mint.url)}
                    {mint === pick && <em className="badge">EcashMesh pick</em>}
                  </span>
                  {mint.reasons.map((reason) => (
                    <span key={reason} className={usable ? "why" : "why bad"}>
                      {reason}
                    </span>
                  ))}
                </span>
              </button>
            );
          })}
        </div>
      ) : (
        <p className="note">
          {candidates.length} mint{candidates.length === 1 ? "" : "s"} to
          compare: {candidates.map(label).join(", ") || "add one above"}.
        </p>
      )}

      {!ranked ? (
        <button
          disabled={!valid || busy !== null}
          onClick={() =>
            run(async () => {
              setBusy("EcashMesh is checking the mints");
              const order = await rankMints(candidates, sats);
              setRanked(order);
              setChosen(
                order.find((mint) => mint.healthy && mint.canReceive)?.url ??
                  null,
              );
            })
          }
        >
          {busy ?? "Compare mints"}
        </button>
      ) : selected ? (
        <>
          <p className="note">
            {props.wallet.label} pays {sats} sats plus its own Lightning fee to{" "}
            {label(selected.url)}. You get a Cashu token for {sats} sats.
          </p>
          <button
            disabled={busy !== null}
            onClick={() =>
              run(async () => {
                const moved = await moveToCashu(
                  props.wallet,
                  selected.url,
                  sats,
                  setBusy,
                );
                setResult({ ...moved, mint: selected.url });
                props.onToken();
              })
            }
          >
            {busy ?? `Pay ${sats} sats from Fedi`}
          </button>
        </>
      ) : (
        <p className="error">No mint can receive right now.</p>
      )}
      {error && <p className="error">{error}</p>}
    </section>
  );
}

const sameHost = (a: string, b: string) => {
  try {
    return new URL(a).host === new URL(b).host;
  } catch {
    return false;
  }
};

function ToFedi(props: { wallet: LightningWallet; onDone(): void }) {
  const [token, setToken] = useState("");
  const [preview, setPreview] = useState<FediPreview | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{
    arrived: number;
    change: string | null;
    changeAmount: number;
  }>();
  let info: TokenInfo | null = null;
  try {
    info = token.trim() ? readToken(token) : null;
  } catch {
    info = null;
  }

  async function run(work: () => Promise<void>) {
    setError(null);
    try {
      await work();
    } catch (failure) {
      setError(describe(failure));
    } finally {
      setBusy(null);
    }
  }

  if (done) {
    return (
      <section className="panel">
        <h2>{done.arrived} sats arrived</h2>
        <p>The mint paid {props.wallet.label}.</p>
        {done.change && (
          <>
            <p>
              Unused fee reserve came back as {done.changeAmount} sats of ecash.
              Copy it into a Cashu wallet:
            </p>
            <TokenBox token={done.change} />
          </>
        )}
        <button
          className="secondary"
          onClick={() => {
            setDone(undefined);
            setPreview(null);
            setToken("");
          }}
        >
          Move more
        </button>
      </section>
    );
  }

  return (
    <section className="panel">
      <h2>Move a Cashu token into Fedi</h2>
      <label>
        Cashu token
        <textarea
          value={token}
          rows={4}
          placeholder="cashuB…"
          spellCheck={false}
          onChange={(event) => {
            setToken(event.target.value);
            setPreview(null);
            setError(null);
          }}
        />
      </label>
      {token.trim() && !info && (
        <p className="error">This is not a Cashu token.</p>
      )}
      {info && !preview && (
        <p className="note">
          {info.amount} {info.unit} from {info.mint}
        </p>
      )}

      {preview ? (
        <>
          <MintCard check={preview.check} need="send" />
          <Evaluation plan={preview.plan} />
          <dl className="summary">
            <dt>Token</dt>
            <dd>{preview.tokenAmount} sats</dd>
            <dt>Mint fee, at most</dt>
            <dd>{preview.feeReserve} sats</dd>
            {preview.inputFee > 0 && (
              <>
                <dt>Mint input fee</dt>
                <dd>{preview.inputFee} sats</dd>
              </>
            )}
            <dt>Arrives in {props.wallet.label}</dt>
            <dd className="strong">{preview.arrives} sats</dd>
          </dl>
          <p className="note">
            Unused fee comes back to you as a change token. If the payment
            fails, the token stays unspent.
          </p>
          <button
            disabled={busy !== null}
            onClick={() =>
              run(async () => {
                const moved = await preview.execute(setBusy);
                setDone({ arrived: preview.arrives, ...moved });
                props.onDone();
              })
            }
          >
            {busy ?? `Move ${preview.arrives} sats to Fedi`}
          </button>
        </>
      ) : (
        <button
          disabled={!info || busy !== null}
          onClick={() =>
            run(async () => {
              setPreview(await previewToFedi(props.wallet, token, setBusy));
            })
          }
        >
          {busy ?? "Plan the move"}
        </button>
      )}
      {error && <p className="error">{error}</p>}
    </section>
  );
}

const FLAGS: Record<string, string> = {
  unknown_reliability: "No payment history with this mint yet",
  unknown_solvency: "The mint's reserves cannot be verified",
  stale_evidence: "Some of what EcashMesh read is out of date",
  unknown_liquidity: "Whether the mint can route this amount is unknown",
};

/** Why EcashMesh approves paying from this mint: its score and evidence. */
function Evaluation({ plan }: { plan: TransferPlan }) {
  const decision = plan.evaluation;
  const source = decision.recommended_source;
  if (!source) return null;
  const flags = [...new Set(source.risk_flags)];
  const meters: [string, number][] = [
    ["Can route this amount", source.liquidity_confidence],
    ["Payment track record", source.reliability_confidence],
    ["Evidence freshness", source.evidence_freshness],
  ];
  const excluded = decision.excluded_sources?.length ?? 0;
  const others = decision.alternative_sources?.length ?? 0;
  return (
    <details className="evaluation" open>
      <summary>
        Why EcashMesh plans this route
        <span>
          {others + excluded === 0
            ? " · the token's mint is the only source that can pay it"
            : ` · ${others} alternative${others === 1 ? "" : "s"}, ${excluded} excluded`}
        </span>
      </summary>
      <p className="note">
        Score {Math.round(source.score)} of 100. Scores rank sources against
        each other; on its own it mostly shows how much EcashMesh knows about
        the mint so far.
      </p>
      {meters.map(([name, value]) => (
        <div key={name} className="meter">
          <span>{name}</span>
          <span className="bar" aria-hidden="true">
            <span style={{ width: `${Math.round(value)}%` }} />
          </span>
          <span className="value">{Math.round(value)}%</span>
        </div>
      ))}
      <ul>
        {decision.explanation.reasons.map((reason) => (
          <li key={reason.code}>{reason.message}</li>
        ))}
        {flags.map((flag) => (
          <li key={flag} className="flag">
            {FLAGS[flag] ?? flag.replace(/_/g, " ")}
          </li>
        ))}
      </ul>
    </details>
  );
}

function MintCard({
  check,
  need,
}: {
  check: MintCheck;
  need: "receive" | "send";
}) {
  const able = need === "receive" ? check.canReceive : check.canSend;
  return (
    <div className={`mint-card ${able ? "ok" : "bad"}`}>
      <strong>
        {able
          ? need === "receive"
            ? "EcashMesh: this mint can receive"
            : "EcashMesh: this mint can pay out"
          : need === "receive"
            ? "EcashMesh: this mint cannot receive now"
            : "EcashMesh: this mint cannot pay out now"}
      </strong>
      <span>
        {check.url} · {check.healthy ? "online" : "not responding as healthy"}
      </span>
      {check.issues.map((issue) => (
        <span key={issue} className="issue">
          {issue}
        </span>
      ))}
    </div>
  );
}

function TokenBox({ token }: { token: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="token">
      <textarea readOnly value={token} rows={4} spellCheck={false} />
      <button
        onClick={() =>
          void navigator.clipboard.writeText(token).then(() => setCopied(true))
        }
      >
        {copied ? "Copied" : "Copy token"}
      </button>
    </div>
  );
}

function PendingBanner(props: { pending: PendingIssue; onDone(): void }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  return (
    <section className="banner">
      <p>
        A payment of {props.pending.amount} sats to {props.pending.mintUrl} may
        not have been turned into ecash yet.
      </p>
      <button
        disabled={busy !== null}
        onClick={async () => {
          setError(null);
          try {
            await issue(props.pending, setBusy);
          } catch (failure) {
            setError(describe(failure));
          } finally {
            setBusy(null);
            props.onDone();
          }
        }}
      >
        {busy ?? "Issue my ecash"}
      </button>
      {error && <p className="error">{error}</p>}
    </section>
  );
}

function Tokens(props: { tokens: SavedToken[]; onForget(): void }) {
  return (
    <section className="panel">
      <h2>Tokens to save</h2>
      <p className="note">
        Kept here only until you copy them into a Cashu wallet. Fedi may clear
        this storage.
      </p>
      {props.tokens.map((saved) => (
        <details key={saved.token}>
          <summary>
            {saved.amount} sats ·{" "}
            {saved.kind === "change" ? "change" : "issued"} ·{" "}
            {new URL(saved.mintUrl).host}
          </summary>
          <TokenBox token={saved.token} />
          <button
            className="secondary"
            onClick={() => {
              forgetToken(saved.token);
              props.onForget();
            }}
          >
            I've saved it, remove
          </button>
        </details>
      ))}
    </section>
  );
}
