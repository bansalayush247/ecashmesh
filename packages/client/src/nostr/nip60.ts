export type Nip60WalletMetadata = {
  mints: Array<{ url: string; unit?: string }>;
};

/** Parses only kind:17375 wallet metadata. Token/proof events are never read. */
export function parseNip60WalletMetadata(
  plaintext: string,
): Nip60WalletMetadata {
  let value: unknown;
  try {
    value = JSON.parse(plaintext);
  } catch {
    throw new Error("The NIP-60 wallet metadata is not valid JSON.");
  }
  if (!Array.isArray(value)) {
    throw new Error("The NIP-60 wallet metadata must be a tag array.");
  }
  const mints = value.flatMap(
    (entry): Array<{ url: string; unit?: string }> => {
      if (
        !Array.isArray(entry) ||
        entry[0] !== "mint" ||
        typeof entry[1] !== "string"
      )
        return [];
      const unit =
        typeof entry[2] === "string" && /^[a-z]{1,8}$/.test(entry[2])
          ? entry[2]
          : undefined;
      return [{ url: entry[1], ...(unit ? { unit } : {}) }];
    },
  );
  if (mints.length === 0) {
    throw new Error("The NIP-60 wallet metadata does not list any mints.");
  }
  return { mints };
}
