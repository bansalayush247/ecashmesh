import { verifyEvent, type Event } from "nostr-tools/pure";
import { canonicalMintUrl } from "./sourceRegistry";

export type Announcement = {
  id: string;
  protocol: "cashu" | "fedimint";
  endpoint: string;
  label: string;
  publisher: string;
  network: string;
};

export function parseAnnouncements(events: readonly Event[]): Announcement[] {
  const seen = new Set<string>();
  return [...events]
    .sort((a, b) => b.created_at - a.created_at || a.id.localeCompare(b.id))
    .flatMap((event) => {
      if (![38172, 38173].includes(event.kind) || !verifyEvent(event))
        return [];
      const d = event.tags.find((tag) => tag[0] === "d")?.[1];
      if (!d) return [];
      const address = event.kind + ":" + event.pubkey + ":" + d;
      if (seen.has(address)) return [];
      seen.add(address);
      const protocol = event.kind === 38172 ? "cashu" : "fedimint";
      if (protocol === "cashu" && d !== event.pubkey) return [];
      const endpoint =
        protocol === "cashu"
          ? canonicalMintUrl(
              event.tags.find((tag) => tag[0] === "u")?.[1] ?? "",
            )
          : /^[a-f0-9]{64}$/i.test(d)
            ? d.toLowerCase()
            : null;
      if (!endpoint) return [];
      let label = endpoint;
      try {
        const metadata: unknown = JSON.parse(event.content);
        if (
          metadata &&
          typeof metadata === "object" &&
          "name" in metadata &&
          typeof metadata.name === "string"
        )
          label = metadata.name.slice(0, 128);
      } catch {}
      // Invites, images, untrusted links and entire announcement content are not imported.
      return [
        {
          id: address,
          protocol,
          endpoint,
          label,
          publisher: event.pubkey,
          network: event.tags.find((tag) => tag[0] === "n")?.[1] ?? "unknown",
        } as Announcement,
      ];
    });
}
