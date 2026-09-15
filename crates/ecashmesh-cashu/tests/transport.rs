use ecashmesh_cashu::{CashuAdapter, MintConfig};
use ecashmesh_core::{Amount, ConnectorHealth, ConnectorId, LightningInvoice};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

async fn mint(statuses: Vec<(u16, &'static str)>) -> (CashuAdapter, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mint/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (status, headers) in statuses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
                assert!(request.len() < 8192);
            }
            let request = String::from_utf8(request).unwrap();
            let first = request.lines().next().unwrap();
            let body = match first {
                "GET /mint/v1/info HTTP/1.1" => include_str!("fixtures/info.json"),
                "GET /mint/v1/keysets HTTP/1.1" => include_str!("fixtures/keysets.json"),
                "GET /mint/v1/keys HTTP/1.1" => include_str!("fixtures/keys.json"),
                other => panic!("Unexpected protocol operation: {other}"),
            };
            let length = if headers.contains("Content-Length:") {
                String::new()
            } else {
                format!("Content-Length: {}\r\n", body.len())
            };
            let response = format!(
                "HTTP/1.1 {status} Fixture\r\n{length}Content-Type: application/json\r\n{headers}Connection: close\r\n\r\n{body}"
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let config = MintConfig::new(ConnectorId::new("cashu:local").unwrap(), &url, 300).unwrap();
    (CashuAdapter::new(config).unwrap(), server)
}

#[tokio::test]
async fn only_public_gets_are_used_and_refresh_failures_retain_stale_evidence() {
    let (adapter, server) = mint(
        vec![(200, ""); 3]
            .into_iter()
            .chain(vec![(503, ""); 3])
            .collect(),
    )
    .await;
    let first = adapter.observe().await;
    assert!(first.metadata.is_known());
    assert!(
        first
            .routing_snapshot(Amount::from_sats(100_000))
            .capabilities
            .can_send
    );
    let failed = adapter.observe().await;
    assert!(failed.metadata.is_stale());
    assert_eq!(failed.metadata.observed_at(), first.metadata.observed_at());
    assert_eq!(failed.metadata.value(), first.metadata.value());
    assert!(failed.input_fees.is_stale());
    assert_eq!(failed.health.value(), Some(&ConnectorHealth::Unavailable));
    assert_eq!(failed.availability.value(), Some(&false));
    assert!(
        !failed
            .routing_snapshot(Amount::from_sats(100_000))
            .capabilities
            .can_send
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cached_http_age_cannot_refresh_stale_capabilities() {
    let (adapter, server) = mint(vec![(200, "Age: 301\r\n"); 3]).await;
    let observed = adapter.observe().await;
    assert!(observed.metadata.is_stale());
    assert!(observed.input_fees.is_stale());
    assert!(
        !observed
            .routing_snapshot(Amount::from_sats(100_000))
            .capabilities
            .can_send
    );
    server.await.unwrap();
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let (adapter, server) =
        mint(vec![(302, "Location: http://127.0.0.1:1/forbidden\r\n"); 3]).await;
    let observed = adapter.observe().await;
    assert!(observed.metadata.is_unknown());
    assert!(
        observed
            .issues
            .iter()
            .any(|issue| issue.message == "HTTP status 302")
    );
    server.await.unwrap();
}

#[tokio::test]
async fn oversized_http_responses_are_explicit_failures() {
    let (adapter, server) = mint(vec![(200, "Content-Length: 2000000\r\n"); 3]).await;
    let observed = adapter.observe().await;
    assert!(observed.metadata.is_unknown());
    assert_eq!(observed.availability.value(), Some(&false));
    assert!(
        observed
            .issues
            .iter()
            .any(|issue| issue.message == "Response exceeds size limit")
    );
    server.await.unwrap();
}

#[test]
fn invalid_configuration_is_rejected_before_network_access() {
    for url in [
        "file:///tmp/mint",
        "ftp://example.com/",
        "https://user:secret@example.com/",
        "https://example.com/?target=other",
        "https://example.com/#fragment",
        "not a URL",
    ] {
        assert!(MintConfig::new(ConnectorId::new("cashu:test").unwrap(), url, 300).is_err());
    }
    assert!(
        MintConfig::new(
            ConnectorId::new("cashu:test").unwrap(),
            "https://example.com/",
            0
        )
        .is_err()
    );
}

async fn quote_adapter(body: &'static str, status: u16) -> (CashuAdapter, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        let request = String::from_utf8(request).unwrap();
        assert_eq!(
            request.lines().next(),
            Some("POST /v1/melt/quote/bolt11 HTTP/1.1")
        );
        let length = request
            .lines()
            .filter_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length").then_some(value)
                })
            })
            .find_map(|value| value.trim().parse::<usize>().ok())
            .unwrap();
        let mut sent = vec![0_u8; length];
        stream.read_exact(&mut sent).await.unwrap();
        assert!(String::from_utf8(sent).unwrap().contains("lnbc1000u"));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let config = MintConfig::new(ConnectorId::new("cashu:quote-test").unwrap(), &url, 300).unwrap();
    (CashuAdapter::new(config).unwrap(), server)
}

#[tokio::test]
async fn unpaid_melt_quotes_are_normalized_or_explicitly_unknown() {
    let invoice = LightningInvoice::parse("lnbc1000u1qqqqqqq9kvtew").unwrap();
    let (adapter, server) = quote_adapter(
        r#"{"quote":"quote-fixture","amount":100000,"fee_reserve":321}"#,
        200,
    )
    .await;
    let observed = adapter
        .melt_quote(&invoice, Amount::from_sats(100_000))
        .await;
    assert_eq!(
        observed.evidence.value().unwrap().fee_reserve.amount.sats(),
        321
    );
    assert!(observed.endpoint.ends_with("/v1/melt/quote/bolt11"));
    assert!(observed.issue.is_none());
    server.await.unwrap();

    let (adapter, server) = quote_adapter("{", 200).await;
    let malformed = adapter
        .melt_quote(&invoice, Amount::from_sats(100_000))
        .await;
    assert!(malformed.evidence.is_unknown());
    assert_eq!(malformed.issue.unwrap().code, "MALFORMED_DATA");
    server.await.unwrap();
}
