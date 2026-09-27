import { useState } from "react";
import { ScrollView, Text } from "react-native";
import {
  addFederationProfile,
  manualCashuProfile,
} from "../nostr/sourceRegistry";
import { Button, Heading, Section, Surface, styles } from "./components";
import { SourceDetails } from "./SourceManager";

/** Explicit opt-in gallery: no live hooks, network requests, signing or routing. */
export function SourceFixtureGallery() {
  const [profiles, setProfiles] = useState(() => [
    ...["A", "B", "C", "D"].map((name) =>
      manualCashuProfile(
        `https://mint-${name.toLowerCase()}.example`,
        `Cashu Mint ${name}`,
      ),
    ),
    ...["A", "B", "C"].flatMap((name, index) =>
      addFederationProfile([], {
        connectorId: `fedimint:fixture-${name}`,
        label: `Fedimint ${name}`,
        federationId: (index + 1).toString(16).padStart(64, "0"),
      }),
    ),
  ]);
  return (
    <ScrollView contentContainerStyle={{ padding: 24, gap: 18 }}>
      <Heading
        eyebrow="FIXTURE GALLERY · OFFLINE · NOT LIVE"
        title="Payment Sources"
      >
        Seven synthetic sources for layout demonstrations. No network, registry
        publishing, routing or execution is available here.
      </Heading>
      <Text style={styles.body}>
        Restart without EXPO_PUBLIC_SOURCE_FIXTURES=true to return to the live
        wallet. Fixtures never populate live registry state.
      </Text>
      {(["cashu", "fedimint"] as const).map((protocol) => (
        <Section key={protocol} title={protocol}>
          {profiles
            .filter((p) => p.protocol === protocol)
            .map((profile) => (
              <Surface key={profile.id}>
                <Text style={styles.subtitle}>{profile.label} · Fixture</Text>
                <Button
                  secondary
                  onPress={() =>
                    setProfiles((current) =>
                      current.map((p) =>
                        p.id === profile.id ? { ...p, enabled: !p.enabled } : p,
                      ),
                    )
                  }
                >
                  {profile.enabled ? "Disable" : "Enable"}
                </Button>
                <SourceDetails profile={profile} />
              </Surface>
            ))}
        </Section>
      ))}
    </ScrollView>
  );
}
