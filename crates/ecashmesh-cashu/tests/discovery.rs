use ecashmesh_cashu::{
    EndpointCapture,
    discovery::{DiscoverySource, MintHint, canonical_mint_url, directory_hints, discover},
};

const NOW: u64 = 1000;
fn hint(url: &str, source: DiscoverySource) -> MintHint {
    MintHint {
        url: url.into(),
        source,
        observed_at: NOW,
        stale: false,
        alias: None,
    }
}
fn directory() -> EndpointCapture {
    EndpointCapture {
        checked_at: NOW,
        observed_at: Some(NOW),
        body: Some(include_str!("fixtures/directory.json").into()),
        error: None,
        stale: false,
    }
}

#[test]
fn canonical_identity_normalizes_equivalent_urls_and_preserves_distinct_mints() {
    for url in [
        "https://MINT.example:443",
        "https://mint.example/",
        "https://mint.example./",
        " https://mint.example/// ",
    ] {
        assert_eq!(canonical_mint_url(url).unwrap(), "https://mint.example/");
    }
    assert_eq!(
        canonical_mint_url("https://mint.example/a/../%7emint").unwrap(),
        "https://mint.example/~mint/"
    );
    assert_eq!(
        canonical_mint_url("https://mint.example/%2E%2e/mint").unwrap(),
        "https://mint.example/mint/"
    );
    for other in [
        "http://mint.example/",
        "https://mint.example:444/",
        "https://mint.example/other/",
    ] {
        assert_ne!(canonical_mint_url(other).unwrap(), "https://mint.example/");
    }
    let once = canonical_mint_url("https://mint.example/%E2%98%83/").unwrap();
    assert_eq!(canonical_mint_url(&once).unwrap(), once);
}

#[test]
fn malformed_and_ambiguous_urls_are_rejected() {
    for url in [
        "file:///etc/passwd",
        "https://user:password@mint.example",
        "https://mint.example/?x=1",
        "https://mint.example/#frag",
        "https://mint.example/%zz",
        "https://mint.example/%2fadmin",
        "https://mint.example/%5cadmin",
        "https://mint.example/\n",
        "not a url",
    ] {
        assert!(canonical_mint_url(url).is_err(), "{url}");
    }
}

#[test]
fn all_sources_merge_by_canonical_url_and_keep_provenance() {
    let hints = [
        DiscoverySource::Seed,
        DiscoverySource::Wallet,
        DiscoverySource::PaymentRequest,
        DiscoverySource::Destination,
        DiscoverySource::Directory("https://directory.example/mints/".into()),
    ]
    .into_iter()
    .map(|source| hint("https://MINT.example:443", source))
    .collect::<Vec<_>>();
    let first = discover(hints.clone(), NOW, 300);
    assert_eq!(first.mints.len(), 1);
    assert_eq!(first.mints[0].provenance.len(), 5);
    assert!(first.mints[0].canonical_id.starts_with("cashu:"));
    assert_eq!(first, discover(hints.into_iter().rev(), NOW, 300));
}

#[test]
fn aliases_are_deterministic_and_conflicting_identities_are_quarantined() {
    let mut a = hint("https://mint.example", DiscoverySource::Seed);
    a.alias = Some("cashu:a".into());
    let mut b = a.clone();
    b.alias = Some("cashu:b".into());
    b.url.push('/');
    let report = discover([b.clone(), a.clone()], NOW, 300);
    assert_eq!(report.mints[0].connector_id().as_str(), "cashu:a");
    assert_eq!(report.mints[0].aliases, ["cashu:a", "cashu:b"]);
    b.url = "https://other.example".into();
    b.alias = a.alias.clone();
    let conflict = discover([a, b], NOW, 300);
    assert!(conflict.mints.is_empty());
    assert!(
        conflict
            .issues
            .iter()
            .all(|issue| issue.code == "CONFLICTING_IDENTITY")
    );
}

#[test]
fn configured_alias_cannot_impersonate_another_mints_generated_id() {
    let other = hint("https://other.example", DiscoverySource::Wallet);
    let other_id = discover([other.clone()], NOW, 300).mints[0]
        .canonical_id
        .clone();
    let mut seed = hint("https://mint.example", DiscoverySource::Seed);
    seed.alias = Some(other_id);
    assert!(discover([seed, other], NOW, 300).mints.is_empty());
}

#[test]
fn directory_fixtures_preserve_partial_stale_and_unavailable_states() {
    let (hints, issues) =
        directory_hints("https://directory.example/mints/", &directory(), NOW, 300);
    assert_eq!(hints.len(), 4);
    assert_eq!(issues[0].code, "MALFORMED_ENTRY");
    let report = discover(hints, NOW, 300);
    assert_eq!(report.mints.len(), 2);
    assert_eq!(report.issues[0].code, "INVALID_MINT_URL");
    let mut failed = directory();
    failed.error = Some("HTTP status 503".into());
    let (hints, issues) = directory_hints("dir", &failed, NOW + 301, 300);
    assert!(hints.iter().all(|hint| hint.stale));
    assert!(
        issues
            .iter()
            .any(|issue| issue.code == "DIRECTORY_UNAVAILABLE")
    );
    failed.body = Some("{broken".into());
    assert!(
        directory_hints("dir", &failed, NOW, 300)
            .1
            .iter()
            .any(|issue| issue.code == "MALFORMED_DIRECTORY")
    );
    failed.body = None;
    failed.observed_at = None;
    assert!(directory_hints("dir", &failed, NOW, 300).0.is_empty());
}

#[test]
fn future_timestamps_stale_claims_and_mint_limit_have_explicit_reasons() {
    let mut future = hint("https://future.example", DiscoverySource::Wallet);
    future.observed_at += 1;
    assert!(discover([future], NOW, 300).mints.is_empty());
    let stale = discover(
        [hint("https://old.example", DiscoverySource::Wallet)],
        NOW + 301,
        300,
    );
    assert!(stale.mints[0].provenance[0].stale);
    assert_eq!(stale.issues[0].code, "STALE_DISCOVERY");
    let hints = (0..70)
        .map(|n| hint(&format!("https://mint{n}.example"), DiscoverySource::Wallet))
        .collect::<Vec<_>>();
    let report = discover(hints.clone(), NOW, 300);
    assert_eq!(report.mints.len(), 64);
    assert_eq!(report, discover(hints.into_iter().rev(), NOW, 300));
    assert_eq!(report.issues[0].code, "LIMIT_REACHED");
}
