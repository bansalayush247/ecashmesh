import assert from "node:assert/strict";
import test from "node:test";
import {
  bindLocalFederation,
  createFederationSetupClient,
} from "../src/host/federationSetup";
import { addFederationProfile } from "../src/nostr/sourceRegistry";

const id = "22".repeat(32);
const local = {
  connector_id: "fedimint:local-name",
  federation_id: id,
  label: "Verified local",
};

test("binding a confirmed local connection replaces a discovered alias without duplication or auto-enabling", () => {
  const previous = addFederationProfile([], {
    connectorId: `fedimint:${id}`,
    federationId: id,
    label: "Discovered",
  });
  previous[0]!.enabled = false;
  previous[0]!.origin = "nostr_nip87";
  const result = bindLocalFederation(previous, local);
  assert.equal(result.length, 1);
  assert.equal(result[0]!.id, local.connector_id);
  assert.equal(result[0]!.enabled, false);
  assert.equal(result[0]!.origin, "nostr_nip87");
  assert.equal(result[0]!.liveStatus, "unknown");
  assert.equal(result[0]!.label, "Discovered");
  assert.deepEqual(bindLocalFederation(result, local), result);
  assert.throws(() =>
    bindLocalFederation(result, { ...local, federation_id: "33".repeat(32) }),
  );
});

test("setup preview is non-joining and only explicit connect sends confirmation; secrets never enter registry", async () => {
  const original = globalThis.fetch;
  const requests: {
    url: string;
    body: Record<string, unknown>;
    headers: Headers;
  }[] = [];
  globalThis.fetch = async (url, options) => {
    requests.push({
      url: String(url),
      body: JSON.parse(String(options?.body ?? "{}")),
      headers: new Headers(options?.headers),
    });
    return Response.json(
      String(url).endsWith("/preview")
        ? {
            federation_id: id,
            confirmation_token: "aa".repeat(32),
            host_label: "Local host",
            expires_in_seconds: 120,
            warning: "New local wallet; no imported funds",
          }
        : {
            ...local,
            status: "connected",
            funded: null,
            wallet_executable: false,
          },
    );
  };
  try {
    const client = createFederationSetupClient("http://127.0.0.1:5000");
    const preview = await client.preview(id, "fed1-private-invite");
    assert.equal(requests.length, 1);
    assert.ok(requests[0]!.url.endsWith("/preview"));
    assert.equal(requests[0]!.body.confirmed, undefined);
    assert.equal(requests[0]!.headers.get("x-ecashmesh-setup"), "1");
    assert.equal(requests[0]!.headers.get("authorization"), null);
    const connected = await client.connect(
      id,
      "fed1-private-invite",
      preview.confirmation_token,
    );
    assert.equal(requests[1]!.body.confirmed, true);
    const profiles = bindLocalFederation([], connected);
    const serialized = JSON.stringify(profiles);
    assert.ok(
      !serialized.includes("private-invite") &&
        !serialized.includes("confirmation_token"),
    );
    assert.ok(
      !serialized.includes("token") && !serialized.includes("clientd_url"),
    );
  } finally {
    globalThis.fetch = original;
  }
});

test("setup rejects mismatched federation responses before a registry can be bound", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = async () =>
    Response.json({
      ...local,
      federation_id: "33".repeat(32),
      status: "connected",
      funded: null,
      wallet_executable: false,
    });
  try {
    await assert.rejects(
      createFederationSetupClient("http://127.0.0.1:5000").connect(
        id,
        "fed1-invite",
        "aa".repeat(32),
      ),
      /mismatch/,
    );
  } finally {
    globalThis.fetch = original;
  }
});
