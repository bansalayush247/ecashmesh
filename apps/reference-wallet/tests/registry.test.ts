import assert from "node:assert/strict";
import test from "node:test";
import {
  finalizeEvent,
  generateSecretKey,
  getPublicKey,
  verifyEvent,
} from "nostr-tools/pure";
import * as nip44 from "nostr-tools/nip44";
import {
  REGISTRY_D,
  loadRegistryEvents,
  restoreRegistry,
  serializeRegistry,
  signRegistry,
  type RegistrySigner,
} from "../src/nostr/registrySync";
import { parseAnnouncements } from "../src/nostr/discovery";
import {
  manualCashuProfile,
  addFederationProfile,
  mergeProfiles,
  type PaymentSourceProfile,
} from "../src/nostr/sourceRegistry";
import { parseNip60WalletMetadata } from "../src/nostr/nip60";
import { collectPayment } from "../src/host/payment";
import { paymentToWire } from "../src/ecashmesh/contracts";
import { createEcashMeshClient } from "../src/ecashmesh/client";
import { EcashMeshError } from "../src/ecashmesh/transport";
import { readEvents } from "../src/nostr/client";

test("metadata network boundary refuses proof subscriptions before connecting", async () => {
  await assert.rejects(
    readEvents([], { kinds: [7375] }),
    /Only source metadata/,
  );
  await assert.rejects(
    readEvents([], { kinds: [17375, 7375] }),
    /Only source metadata/,
  );
});

function signer() {
  // Disposable test-only identity, never a user's private key.
  const key = generateSecretKey(),
    pubkey = getPublicKey(key);
  const conversation = nip44.v2.utils.getConversationKey(key, pubkey);
  return {
    key,
    session: {
      pubkey,
      nip44Encrypt: async (plaintext) =>
        nip44.v2.encrypt(plaintext, conversation),
      nip44Decrypt: async (ciphertext) =>
        nip44.v2.decrypt(ciphertext, conversation),
      signRegistry: async (template) => finalizeEvent(template, key),
    } satisfies RegistrySigner,
  };
}
const source = () => manualCashuProfile("https://mint.example", "Mint");

test("rapid saves use distinct timestamps so the last explicit save restores", async () => {
  const { session } = signer();
  const first = await signRegistry([source()], session);
  const second = await signRegistry([], session);
  assert.ok(second.created_at > first.created_at);
  assert.deepEqual(await loadRegistryEvents([first, second], session), []);
});

test("signed NIP-78 registry is encrypted to connected identity and restores without live claims", async () => {
  const { session } = signer();
  const profiles = [
    {
      ...source(),
      liveStatus: "online" as const,
      evidenceFreshness: "fresh" as const,
    },
  ];
  const event = await signRegistry(profiles, session);
  assert.equal(event.kind, 30078);
  assert.deepEqual(event.tags, [["d", REGISTRY_D]]);
  assert.equal(event.pubkey, session.pubkey);
  assert.ok(verifyEvent(event));
  assert.equal(event.content.includes("mint.example"), false);
  const restored = await loadRegistryEvents([event], session);
  assert.equal(restored?.[0]?.endpoint, "https://mint.example");
  assert.equal(restored?.[0]?.liveStatus, "unknown");
  assert.equal(restored?.[0]?.evidenceFreshness, "unknown");
});

test("projection never sends proofs, private keys, clientd tokens or federation credentials to Nostr or API", async () => {
  const secrets = [
    "proof-secret-test",
    "proof-C-test",
    "nsec1test",
    "clientd-token-test",
    "federation-private-test",
    "wallet-private-test",
  ];
  const contaminated = {
    ...source(),
    proofs: [{ secret: secrets[0], C: secrets[1] }],
    nsec: secrets[2],
    token: secrets[3],
    credentials: secrets[4],
    privateKey: secrets[5],
    observation: { secret: secrets[0] },
  };
  const { session } = signer();
  const event = await signRegistry([contaminated], session);
  const plaintext = await session.nip44Decrypt(event.content);
  const apiPayload = JSON.stringify(
    paymentToWire(
      collectPayment(
        "1000",
        "invoice",
        "lightning",
        undefined,
        [contaminated],
        true,
      ),
    ),
  );
  for (const secret of secrets) {
    assert.ok(!plaintext.includes(secret));
    assert.ok(!apiPayload.includes(secret));
  }
  const parsed = parseNip60WalletMetadata(
    JSON.stringify([
      ["privkey", secrets[5]],
      ["mint", "https://mint.example"],
      ["proofs", secrets[0]],
      ["nsec", secrets[2]],
    ]),
  );
  for (const secret of secrets)
    assert.ok(!JSON.stringify(parsed).includes(secret));
});

test("no encryption or a mismatched signer fails closed without plaintext fallback", async () => {
  const { session } = signer(),
    wrong = signer();
  await assert.rejects(
    signRegistry([source()], { ...session, nip44Encrypt: undefined }),
    /Save locally/,
  );
  await assert.rejects(
    signRegistry([source()], {
      ...session,
      signRegistry: wrong.session.signRegistry,
    }),
    /mismatched/,
  );
});

test("latest valid registry is authoritative, not merged; malformed newer event is ignored", async () => {
  const { session, key } = signer();
  const older = await signRegistry([source()], session, 10);
  const newer = await signRegistry([], session, 20);
  const bad = finalizeEvent(
    {
      kind: 30078,
      tags: [["d", REGISTRY_D]],
      created_at: 30,
      content: "bad-encryption",
    },
    key,
  );
  assert.deepEqual(
    await loadRegistryEvents([older, bad, newer, older], session),
    [],
  );
  assert.equal(await loadRegistryEvents([], session), null);
  await assert.rejects(loadRegistryEvents([bad], session), /retained/);
  await assert.rejects(
    loadRegistryEvents([older], {
      ...session,
      nip44Decrypt: async () => {
        throw new Error("declined");
      },
    }),
    /retained/,
  );
});

test("wrong author, d tag, kind, or tampered signature cannot restore registry", async () => {
  const { session, key } = signer();
  const event = await signRegistry([source()], session);
  const other = await signRegistry([source()], signer().session);
  const wrongD = finalizeEvent({ ...event, tags: [["d", "another-app"]] }, key);
  const wrongKind = finalizeEvent({ ...event, kind: 7375 }, key);
  const tampered = JSON.parse(JSON.stringify(event));
  tampered.sig = "00".repeat(64);
  assert.equal(
    await loadRegistryEvents([other, wrongD, wrongKind, tampered], session),
    null,
  );
});

test("version migration boundary rejects malformed, future, duplicate or credential-bearing registries", () => {
  const content = JSON.parse(serializeRegistry([source()]));
  for (const value of [
    "not json",
    JSON.stringify({ ...content, version: 2 }),
    JSON.stringify({ version: 0, sources: [] }),
    JSON.stringify({
      ...content,
      sources: [...content.sources, ...content.sources],
    }),
    JSON.stringify({
      ...content,
      sources: [
        { ...content.sources[0], endpoint: "https://user:secret@mint.example" },
      ],
    }),
    JSON.stringify({
      ...content,
      sources: [{ ...content.sources[0], label: "nsec1test" }],
    }),
  ])
    assert.throws(() => restoreRegistry(value));
});

test("NIP-87 parses signed public announcements separately and never imports federation invite credentials", () => {
  const { key, session } = signer();
  const cashu = finalizeEvent(
    {
      kind: 38172,
      created_at: 1,
      tags: [
        ["d", session.pubkey],
        ["u", "https://Mint.example/"],
        ["n", "mainnet"],
      ],
      content: '{"name":"Mint A"}',
    },
    key,
  );
  const fed = finalizeEvent(
    {
      kind: 38173,
      created_at: 2,
      tags: [
        ["d", "11".repeat(32)],
        ["u", "fed11-private-invite"],
      ],
      content: '{"name":"Fed A"}',
    },
    key,
  );
  const found = parseAnnouncements([cashu, fed, cashu]);
  assert.equal(found.length, 2);
  assert.ok(found.every((p) => !("enabled" in p) && !("authorization" in p)));
  assert.ok(!JSON.stringify(found).includes("fed11-private-invite"));
  assert.ok(found.some((p) => p.endpoint === "https://mint.example"));
  const tampered = JSON.parse(JSON.stringify(cashu));
  tampered.sig = "00".repeat(64);
  assert.deepEqual(parseAnnouncements([tampered]), []);
});

test("manual and NIP-60/NIP-87 references deduplicate by endpoint without re-enabling disabled source", () => {
  const old = { ...source(), enabled: false };
  const merged = mergeProfiles(
    [old],
    [{ ...source(), id: "different-id", origin: "nostr_nip87" }],
  );
  assert.equal(merged.length, 1);
  assert.equal(merged[0]?.enabled, false);
});

test("automatic five Cashu plus three Fedimint forwards all eligible identities and no seed fallback", () => {
  const profiles = [
    ...Array.from({ length: 5 }, (_, i) =>
      manualCashuProfile(`https://mint-${i}.example`, `Mint ${i}`),
    ),
    ...Array.from(
      { length: 3 },
      (_, i) =>
        addFederationProfile([], {
          connectorId: `fedimint:fed-${i}`,
          federationId: i.toString(16).padStart(64, "0"),
          label: `Fed ${i}`,
        })[0]!,
    ),
  ];
  for (const type of ["lightning", "cashu"] as const) {
    const wire = paymentToWire(
      collectPayment(
        "1000",
        type === "cashu" ? "https://destination.example" : "invoice",
        type,
        undefined,
        profiles,
        true,
      ),
    );
    assert.equal(wire.wallet_mint_urls?.length, 5);
    assert.equal(wire.federation_connector_ids?.length, 3);
    assert.ok(!wire.wallet_mint_urls?.includes("https://destination.example"));
    assert.equal(wire.strict_source_registry, true);
  }
  const filtered = profiles.map((p, i) =>
    i < 5
      ? { ...p, enabled: false }
      : { ...p, authorization: "discovered_only" },
  ) as PaymentSourceProfile[];
  const wire = paymentToWire(
    collectPayment(
      "1000",
      "invoice",
      "lightning",
      "https://ignored.example",
      filtered,
      true,
    ),
  );
  assert.deepEqual(wire.wallet_mint_urls, []);
  assert.deepEqual(wire.federation_connector_ids, []);
});

test("no-route response preserves structured source and quote diagnostics", async () => {
  const diagnostics = {
    excluded_sources: [
      { source_id: "fedimint:a", reason: "No read-only quote bridge" },
    ],
    connector_observations: [],
    quote_observations: [],
  };
  const client = createEcashMeshClient({
    baseUrl: "http://local",
    fetch: async () =>
      new Response(
        JSON.stringify({
          error: { code: "NO_VIABLE_ROUTE", message: "No route", diagnostics },
        }),
        { status: 422 },
      ),
  });
  await assert.rejects(
    client.evaluateRoute(collectPayment("1000", "invoice")),
    (error: unknown) => {
      assert.ok(error instanceof EcashMeshError);
      assert.deepEqual(error.diagnostics, diagnostics);
      return true;
    },
  );
});
