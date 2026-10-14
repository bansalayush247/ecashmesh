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
} from "./flows";
import {
  forgetToken,
  pendingIssue,
  type PendingIssue,
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
  const [mintUrl, setMintUrl] = useState(props.suggestions[0]?.url ?? "");
  const [amount, setAmount] = useState("1000");
  const [check, setCheck] = useState<MintCheck | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{ token: string; amount: number }>();
  const sats = Number.parseInt(amount, 10);
  const valid = mintUrl.trim() !== "" && Number.isSafeInteger(sats) && sats > 0;

  const reset = () => {
    setCheck(null);
    setError(null);
    setResult(undefined);
  };

  async function run<T>(work: () => Promise<T>) {
    setError(null);
    try {
      return await work();
    } catch (failure) {
      setError(describe(failure));
      return undefined;
    } finally {
      setBusy(null);
    }
  }

  if (result) {
    return (
      <section className="panel">
        <h2>Your Cashu token</h2>
        <p>
          {result.amount} sats of ecash at {check?.url ?? mintUrl}. Copy it into
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
        Mint
        <input
          value={mintUrl}
          list="mint-suggestions"
          placeholder="https://mint.example"
          inputMode="url"
          onChange={(event) => {
            setMintUrl(event.target.value);
            reset();
          }}
        />
        <datalist id="mint-suggestions">
          {props.suggestions.map((mint) => (
            <option key={mint.url} value={mint.url}>
              {mint.label}
            </option>
          ))}
        </datalist>
      </label>
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

      {check && <MintCard check={check} need="receive" />}

      {!check || !check.canReceive ? (
        <button
          disabled={!valid || busy !== null}
          onClick={() =>
            run(async () => {
              setBusy("EcashMesh is checking the mint");
              setCheck(await checkMint(mintUrl.trim(), sats));
            })
          }
        >
          {busy ?? "Check mint"}
        </button>
      ) : (
        <>
          <p className="note">
            {props.wallet.label} pays {sats} sats plus its own Lightning fee.
            You get a Cashu token for {sats} sats.
          </p>
          <button
            disabled={busy !== null}
            onClick={() =>
              run(async () => {
                const moved = await moveToCashu(
                  props.wallet,
                  check.url,
                  sats,
                  setBusy,
                );
                setResult(moved);
                props.onToken();
              })
            }
          >
            {busy ?? `Pay ${sats} sats from Fedi`}
          </button>
        </>
      )}
      {error && <p className="error">{error}</p>}
    </section>
  );
}

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
