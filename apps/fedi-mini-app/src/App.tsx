import type { PaymentSource, RouteDecision } from "@ecashmesh/client/http";
import { Fragment, useEffect, useState } from "react";

import { readToken } from "./cashu";
import { DecisionView, humanize, SourceDetails } from "./decision";
import {
  apiUrl,
  describe,
  type Holding,
  holdingFor,
  holdings,
  issue,
  moveToCashu,
  payFromMint,
  rankForFedi,
  rankMints,
  type RankedMint,
  type Ranking,
  spendable,
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
            <ToFedi
              wallet={wallet}
              mints={lab.mints}
              saved={tokens}
              onDone={refresh}
            />
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
  const [inspecting, setInspecting] = useState<string | null>(null);
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
        <div className="ranking" aria-label="Mints">
          <p className="note">
            EcashMesh&apos;s order: online and able to receive first, then the
            lowest fee for spending the ecash later.
          </p>
          {ranked.map((mint, index) => {
            const usable = mint.healthy && mint.canReceive;
            return (
              <div
                key={mint.url}
                className={`source-card${chosen === mint.url ? " chosen" : ""}${usable ? "" : " excluded"}`}
              >
                <div className="source-top">
                  <span className="rank">{usable ? index + 1 : "–"}</span>
                  <span className="rank-body">
                    <span className="rank-title">
                      {label(mint.url)}
                      {mint === pick && <em className="badge">Top match</em>}
                    </span>
                    {mint.reasons.map((reason) => (
                      <span key={reason} className={usable ? "why" : "why bad"}>
                        {reason}
                      </span>
                    ))}
                  </span>
                </div>
                <div className="source-actions">
                  <button
                    className="secondary"
                    aria-expanded={inspecting === mint.url}
                    onClick={() =>
                      setInspecting(inspecting === mint.url ? null : mint.url)
                    }
                  >
                    {inspecting === mint.url ? "Hide details" : "Inspect"}
                  </button>
                  {usable && (
                    <button
                      aria-pressed={chosen === mint.url}
                      onClick={() => setChosen(mint.url)}
                    >
                      {chosen === mint.url ? "Chosen" : "Receive here"}
                    </button>
                  )}
                </div>
                {inspecting === mint.url && <MintInspector mint={mint} />}
              </div>
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

function ToFedi(props: {
  wallet: LightningWallet;
  mints: { url: string; label: string }[];
  saved: SavedToken[];
  onDone(): void;
}) {
  const name = (url: string) =>
    props.mints.find((mint) => sameHost(mint.url, url))?.label;
  const mintName = (url: string) => name(url) ?? mintHost(url);
  const [tokens, setTokens] = useState<string[]>([]);
  const [held, setHeld] = useState<Holding[]>([]);
  const [draft, setDraft] = useState("");
  const [amount, setAmount] = useState("");
  const [ranking, setRanking] = useState<Ranking | null>(null);
  const [chosen, setChosen] = useState<PaymentSource | null>(null);
  const [inspecting, setInspecting] = useState<PaymentSource | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{
    arrived: number;
    mint: string;
    change: string | null;
    changeAmount: number;
  }>();
  const sats = Number.parseInt(amount, 10);
  const total = held.reduce((sum, holding) => sum + holding.amount, 0);
  const chosenHolding = chosen ? holdingFor(held, chosen) : undefined;

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

  const [notice, setNotice] = useState<string | null>(null);
  const unused = props.saved.filter((saved) => !tokens.includes(saved.token));

  function addTokens(added: string[]) {
    void run(async () => {
      setNotice(null);
      const fresh = added.map((token) => token.trim()).filter(Boolean);
      for (const token of fresh) readToken(token);
      if (fresh.every((token) => tokens.includes(token))) {
        throw new Error("Already added.");
      }
      setBusy("Checking the tokens at their mints");
      const next = [...new Set([...tokens, ...fresh])];
      const { held: grouped, spent } = await holdings(next);
      setTokens(next.filter((token) => !spent.includes(token)));
      setHeld(grouped);
      setDraft("");
      setRanking(null);
      setChosen(null);
      if (spent.length > 0) {
        setNotice(
          `${spent.length} token${spent.length === 1 ? " was" : "s were"} already spent and ${spent.length === 1 ? "was" : "were"} left out.`,
        );
        props.onDone();
      }
      if (grouped.length > 0) {
        // Suggest the most one mint could send, leaving room for its fees.
        const best = Math.max(...grouped.map(spendable));
        setAmount(
          String(Math.max(1, best - Math.max(2, Math.ceil(best / 50)))),
        );
      }
    });
  }

  if (done) {
    return (
      <section className="panel">
        <h2>{done.arrived} sats arrived</h2>
        <p>
          {done.mint} paid {props.wallet.label}.
        </p>
        {done.change && (
          <>
            <p>
              The rest of the spent tokens, {done.changeAmount} sats including
              unused fee reserve, came back as new ecash. Copy it into a Cashu
              wallet:
            </p>
            <TokenBox token={done.change} />
          </>
        )}
        <button
          className="secondary"
          onClick={() => {
            setDone(undefined);
            setRanking(null);
            setChosen(null);
            setTokens([]);
            setHeld([]);
            setAmount("");
          }}
        >
          Move more
        </button>
      </section>
    );
  }

  if (ranking?.decision && inspecting) {
    return (
      <section className="panel">
        <SourceDetails
          decision={ranking.decision}
          source={inspecting}
          onClose={() => setInspecting(null)}
          name={name}
        />
      </section>
    );
  }

  return (
    <section className="panel">
      <h2>Move Cashu tokens into Fedi</h2>
      <p className="note">
        Add tokens from one mint or several. EcashMesh ranks the mints that
        could pay, the way it ranks a wallet&apos;s sources.
      </p>
      <label>
        Cashu token
        <textarea
          value={draft}
          rows={3}
          placeholder="cashuB…"
          spellCheck={false}
          onChange={(event) => setDraft(event.target.value)}
        />
      </label>
      <button
        className="secondary"
        disabled={!draft.trim() || busy !== null}
        onClick={() => addTokens([draft])}
      >
        Add token
      </button>

      {unused.length > 0 && (
        <div className="saved-picker">
          <span className="note">Tokens saved in this app</span>
          {unused.map((saved) => (
            <div key={saved.token} className="row">
              <span>
                {saved.amount} sats · {mintName(saved.mintUrl)}
              </span>
              <button
                className="secondary small"
                disabled={busy !== null}
                onClick={() => addTokens([saved.token])}
              >
                Add
              </button>
            </div>
          ))}
          {unused.length > 1 && (
            <button
              className="secondary"
              disabled={busy !== null}
              onClick={() => addTokens(unused.map((saved) => saved.token))}
            >
              Add all {unused.length} saved tokens
            </button>
          )}
        </div>
      )}
      {notice && <p className="note">{notice}</p>}

      {held.length > 0 && (
        <dl className="summary">
          {held.map((holding) => (
            <Fragment key={holding.mint}>
              <dt>
                {mintName(holding.mint)} · {holding.tokens.length} token
                {holding.tokens.length === 1 ? "" : "s"}
              </dt>
              <dd>{holding.amount} sats</dd>
            </Fragment>
          ))}
          <dt>Total</dt>
          <dd className="strong">{total} sats</dd>
        </dl>
      )}

      {held.length > 0 && (
        <label>
          Amount to move into Fedi (sats)
          <input
            value={amount}
            inputMode="numeric"
            onChange={(event) => {
              setAmount(event.target.value.replace(/\D/g, ""));
              setRanking(null);
              setChosen(null);
            }}
          />
        </label>
      )}

      {ranking && (
        <>
          {ranking.decision ? (
            <DecisionView
              decision={ranking.decision}
              excluded={ranking.excluded}
              chosen={chosen?.route_id ?? null}
              onChoose={setChosen}
              onInspect={setInspecting}
              name={name}
            />
          ) : (
            <>
              <p className="error">{ranking.failure}</p>
              <DecisionView
                decision={emptyDecision}
                excluded={ranking.excluded}
                chosen={null}
                onChoose={() => undefined}
                onInspect={() => undefined}
              />
            </>
          )}
        </>
      )}

      {held.length > 0 && !ranking && (
        <button
          disabled={!(sats > 0) || busy !== null}
          onClick={() =>
            run(async () => {
              const ranked = await rankForFedi(
                props.wallet,
                held,
                sats,
                setBusy,
              );
              setRanking(ranked);
              setChosen(ranked.decision?.recommended_source ?? null);
            })
          }
        >
          {busy ?? "Rank my mints with EcashMesh"}
        </button>
      )}

      {ranking && chosen && chosenHolding && (
        <>
          <p className="note">
            {mintName(chosenHolding.mint)} pays {ranking.amount} sats into{" "}
            {props.wallet.label}, plus at most {chosen.fee.amount ?? "?"} sats
            fee reserve
            {chosenHolding.inputFee > 0
              ? ` and ${chosenHolding.inputFee} sats input fee`
              : ""}
            . Unused reserve comes back as a change token; if the payment fails
            the tokens stay unspent.
          </p>
          <button
            disabled={busy !== null}
            onClick={() =>
              run(async () => {
                const moved = await payFromMint(
                  props.wallet,
                  chosenHolding,
                  ranking,
                  setBusy,
                );
                setDone({
                  arrived: ranking.amount,
                  mint: mintName(chosenHolding.mint),
                  change: moved.change,
                  changeAmount: moved.changeAmount,
                });
                props.onDone();
              })
            }
          >
            {busy ?? `Move ${ranking.amount} sats to Fedi`}
          </button>
        </>
      )}
      {ranking && (
        <button
          className="secondary"
          disabled={busy !== null}
          onClick={() => {
            setRanking(null);
            setChosen(null);
          }}
        >
          Change amount or tokens
        </button>
      )}
      {error && <p className="error">{error}</p>}
    </section>
  );
}

const mintHost = (url: string) => {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
};

/** Shape for showing exclusions when no mint can pay at all. */
const emptyDecision = {
  quote_id: "none",
  alternative_sources: [],
  score_breakdown: {
    liquidity: 0,
    reliability: 0,
    evidence_freshness: 0,
    fees: null,
    risk_penalty: 0,
  },
  risk_flags: [],
  evidence: [],
  explanation: { summary: "", reasons: [], alternative_weaknesses: [] },
  expires_at: "",
  expires_at_unix_seconds: 0,
} as RouteDecision;

/** Everything EcashMesh observed about one mint. */
function MintInspector({ mint }: { mint: RankedMint }) {
  const [raw, setRaw] = useState(false);
  const observation = (mint.observed.observation ?? {}) as Record<
    string,
    { value?: unknown; state?: string; freshness?: string } | undefined
  >;
  const value = (key: string) => observation[key]?.value;
  const keysets = value("input_fees");
  const endpoints = (
    mint.observed.observation as { endpoints?: Record<string, string> }
  )?.endpoints;
  return (
    <div className="inspector inline">
      <div className="row">
        <span>Health</span>
        <span>{String(value("health") ?? "unknown")}</span>
      </div>
      <div className="row">
        <span>Reachable</span>
        <span>{value("availability") === true ? "Yes" : "No"}</span>
      </div>
      <div className="row">
        <span>Receives / pays out Lightning</span>
        <span>
          {mint.canReceive ? "Yes" : "No"} / {mint.canSend ? "Yes" : "No"}
        </span>
      </div>
      <div className="row">
        <span>Active keysets (input fee)</span>
        <span>
          {Array.isArray(keysets)
            ? (keysets as { active: boolean; input_fee_ppk: number }[])
                .filter((keyset) => keyset.active)
                .map((keyset) => `${keyset.input_fee_ppk} ppk`)
                .join(", ") || "none"
            : "Unknown"}
        </span>
      </div>
      {endpoints &&
        Object.entries(endpoints).map(([name, url]) => (
          <div key={name} className="row">
            <span>{humanize(name)}</span>
            <code>{url}</code>
          </div>
        ))}
      {mint.issues.map((issue) => (
        <p key={issue} className="why bad">
          {issue}
        </p>
      ))}
      <button className="secondary" onClick={() => setRaw(!raw)}>
        {raw ? "Hide response JSON" : "Inspect response JSON"}
      </button>
      {raw && (
        <pre className="raw">{JSON.stringify(mint.observed, null, 2)}</pre>
      )}
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
