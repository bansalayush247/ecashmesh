use ecashmesh_cashu::discovery::{DiscoveryService, DiscoverySource, MintHint};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

struct Server {
    url: String,
    requests: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(directory: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let mint_url = url.clone();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        let mut directory_reads = 0;
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                bytes.push(stream.read_u8().await.unwrap());
                assert!(bytes.len() < 8192);
            }
            count.fetch_add(1, Ordering::Relaxed);
            let request = String::from_utf8(bytes).unwrap();
            let mut status = 200;
            let body = match request.lines().next().unwrap() {
                "GET /mints/ HTTP/1.1" if directory => {
                    directory_reads += 1;
                    if directory_reads > 1 {
                        status = 503;
                    }
                    json!([{"url":mint_url}, {"url":mint_url.trim_end_matches('/')}]).to_string()
                }
                "GET /v1/info HTTP/1.1" => include_str!("fixtures/info.json").into(),
                "GET /v1/keysets HTTP/1.1" => include_str!("fixtures/keysets.json").into(),
                "GET /v1/keys HTTP/1.1" => include_str!("fixtures/keys.json").into(),
                other => panic!("Unexpected operation: {other}"),
            };
            stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    Server {
        url,
        requests,
        task,
    }
}
fn hint(url: &str, source: DiscoverySource) -> MintHint {
    MintHint {
        url: url.into(),
        source,
        observed_at: 0,
        stale: false,
        alias: None,
    }
}

#[tokio::test]
async fn directory_discovery_deduplicates_before_fetch_and_retains_stale_directory_on_failure() {
    let server = server(true).await;
    let service = DiscoveryService::new(
        Vec::new(),
        vec![format!("{}mints/", server.url)],
        vec![server.url.clone()],
        300,
    )
    .unwrap();
    let state = service
        .collect(vec![hint(&server.url, DiscoverySource::Wallet)])
        .await
        .unwrap();
    assert_eq!(state.report.mints.len(), 1);
    assert_eq!(state.observations.len(), 1);
    assert_eq!(server.requests.load(Ordering::Relaxed), 4); // directory plus 3 public GETs
    assert!(state.observations[0].public_keysets.is_known());
    let second = service.collect(Vec::new()).await.unwrap();
    assert_eq!(second.report.mints.len(), 1);
    assert!(
        second.report.mints[0]
            .provenance
            .iter()
            .all(|hint| hint.stale)
    );
    assert!(
        second
            .report
            .issues
            .iter()
            .any(|issue| issue.code == "DIRECTORY_UNAVAILABLE")
    );
    assert!(second.observations[0].metadata.is_known()); // fresh probe distinct from stale discovery
}

#[tokio::test]
async fn wallet_and_directory_urls_cannot_contact_unapproved_local_services() {
    let server = server(false).await;
    let service = DiscoveryService::new(Vec::new(), Vec::new(), Vec::new(), 300).unwrap();
    let state = service
        .collect(vec![hint(&server.url, DiscoverySource::Wallet)])
        .await
        .unwrap();
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    assert_eq!(state.observations[0].availability.value(), Some(&false));
    assert!(
        state.observations[0]
            .issues
            .iter()
            .any(|issue| issue.message.contains("non-public"))
    );
    assert!(
        service
            .collect(Vec::new())
            .await
            .unwrap()
            .observations
            .is_empty()
    ); // request hints do not persist
}

#[tokio::test]
async fn configured_seeds_and_all_request_sources_share_one_probe() {
    let server = server(false).await;
    let mut seed = hint(&server.url, DiscoverySource::Seed);
    seed.alias = Some("cashu:seed".into());
    let service = DiscoveryService::new(vec![seed], Vec::new(), Vec::new(), 300).unwrap();
    let state = service
        .collect(
            [
                DiscoverySource::Wallet,
                DiscoverySource::PaymentRequest,
                DiscoverySource::Destination,
            ]
            .into_iter()
            .map(|source| hint(&server.url, source))
            .collect(),
        )
        .await
        .unwrap();
    assert_eq!(state.report.mints.len(), 1);
    assert_eq!(state.report.mints[0].provenance.len(), 4);
    assert_eq!(state.observations[0].id.as_str(), "cashu:seed");
    assert_eq!(server.requests.load(Ordering::Relaxed), 3);
}
