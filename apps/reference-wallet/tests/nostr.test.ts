import assert from "node:assert/strict";
import test from "node:test";
import { parseNip60WalletMetadata } from "../src/nostr/nip60";
import {
  addFederationProfile,
  cashuProfilesFromNip60,
  enabledCashuMintUrls,
  enabledFederationConnectorIds,
  setProfileEnabled,
} from "../src/nostr/sourceRegistry";

test("NIP-60 wallet metadata extracts and canonicalizes multiple mint URLs without proofs", () => {
  const metadata = parseNip60WalletMetadata(
    JSON.stringify([
      ["privkey", "never-read-by-ecashmesh"],
      ["mint", "https://Mint.Example/"],
      ["mint", "https://mint.example"],
      ["mint", "https://second.example/path/", "sat"],
    ]),
  );
  const profiles = cashuProfilesFromNip60(metadata.mints);
  assert.deepEqual(
    profiles.map((profile) => [profile.endpoint, profile.unit]),
    [
      ["https://mint.example", undefined],
      ["https://second.example/path", "sat"],
    ],
  );
  assert.ok(profiles.every((profile) => profile.origin === "nostr_nip60"));
  assert.ok(
    profiles.every((profile) => profile.authorization === "user_authorized"),
  );
  assert.equal(
    JSON.stringify(profiles).includes("never-read-by-ecashmesh"),
    false,
  );
});

test("NIP-60 malformed or proof-shaped payloads are rejected", () => {
  assert.throws(() => parseNip60WalletMetadata("not json"), /valid JSON/);
  assert.throws(
    () =>
      parseNip60WalletMetadata(
        JSON.stringify({
          mint: "https://mint.example",
          proofs: [{ secret: "no" }],
        }),
      ),
    /tag array/,
  );
  assert.throws(
    () => parseNip60WalletMetadata(JSON.stringify([["privkey", "x"]])),
    /does not list any mints/,
  );
});

test("explicit Fedimint profiles contain no clientd credential and can be disabled", () => {
  const federation = addFederationProfile([], {
    connectorId: "fedimint:bitcoin-principles",
    label: "Bitcoin Principles",
    federationId:
      "b21068c84f5b12ca4fdf93f3e443d3bd7c27e8642d0d52ea2e4dce6fdbbee9df",
  });
  assert.deepEqual(enabledFederationConnectorIds(federation), [
    "fedimint:bitcoin-principles",
  ]);
  assert.equal(JSON.stringify(federation).includes("token"), false);
  assert.equal(
    JSON.stringify(
      setProfileEnabled(federation, federation[0]!.id, false),
    ).includes('enabled":false'),
    true,
  );
});

test("enabled Cashu source URLs are the only Cashu data forwarded for evaluation", () => {
  const profiles = cashuProfilesFromNip60([{ url: "https://mint.example" }]);
  assert.deepEqual(enabledCashuMintUrls(profiles), ["https://mint.example"]);
  assert.deepEqual(
    enabledCashuMintUrls(setProfileEnabled(profiles, profiles[0]!.id, false)),
    [],
  );
});
