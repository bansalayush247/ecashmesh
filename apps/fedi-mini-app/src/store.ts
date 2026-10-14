// What must survive the mini app closing mid-move: a paid mint quote whose
// ecash is not yet issued, and tokens the user has not yet saved elsewhere.
// Fedi can clear a mini app's storage, so tokens are shown to be copied at
// once; storage is only the safety net for a crash in between.

export type PendingIssue = {
  mintUrl: string;
  quoteId: string;
  amount: number;
  createdAt: number;
};
export type SavedToken = {
  token: string;
  mintUrl: string;
  amount: number;
  /** "issued" from Fedi → Cashu, "change" left over from Cashu → Fedi. */
  kind: "issued" | "change";
  createdAt: number;
};

const PENDING = "ecashmesh.pending-issue";
const TOKENS = "ecashmesh.tokens";

function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: unknown) {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Storage unavailable: the flow still shows the token to copy.
  }
}

export const pendingIssue = () => read<PendingIssue | null>(PENDING, null);
export const setPendingIssue = (pending: PendingIssue | null) =>
  write(PENDING, pending);

export const savedTokens = () => read<SavedToken[]>(TOKENS, []);
export function saveToken(token: SavedToken) {
  write(TOKENS, [token, ...savedTokens()]);
}
export function forgetToken(token: string) {
  write(
    TOKENS,
    savedTokens().filter((saved) => saved.token !== token),
  );
}

const MINTS = "ecashmesh.mints";

/** Mints the user added, offered again next time. */
export const recentMints = () => read<string[]>(MINTS, []);
export function rememberMint(url: string) {
  write(
    MINTS,
    [url, ...recentMints().filter((other) => other !== url)].slice(0, 8),
  );
}
