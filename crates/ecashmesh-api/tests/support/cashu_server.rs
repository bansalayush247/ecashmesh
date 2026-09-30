use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub struct ApiServer {
    child: Child,
    address: String,
}

impl ApiServer {
    #[allow(dead_code)]
    pub fn start(mint_url: &str) -> Self {
        Self::start_with_sources(
            &json!([{"id":"cashu:fixture","url":mint_url}]),
            &json!([]),
            &json!([]),
        )
    }

    pub fn start_with_sources(seeds: &Value, directories: &Value, allowed: &Value) -> Self {
        Self::start_with_federations(seeds, directories, allowed, &json!([]))
    }

    pub fn start_with_federations(
        seeds: &Value,
        directories: &Value,
        allowed: &Value,
        federations: &Value,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_ecashmesh-api"))
            .env("ECASHMESH_API_ADDRESS", &address)
            .env("ROUTING_MODE", "live")
            .env("ECASHMESH_CASHU_MAX_AGE_SECONDS", "300")
            .env("ECASHMESH_CASHU_MINTS", seeds.to_string())
            .env("ECASHMESH_CASHU_DIRECTORIES", directories.to_string())
            .env("ECASHMESH_CASHU_ALLOWED_MINTS", allowed.to_string())
            .env("ECASHMESH_FEDIMINT_FEDERATIONS", federations.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let server = Self { child, address };
        for _ in 0..100 {
            if TcpStream::connect(&server.address).is_ok() {
                return server;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("API failed to start");
    }

    pub fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        self.request("POST", path, &body.to_string())
    }

    #[allow(dead_code)]
    pub fn get(&self, path: &str) -> (u16, Value) {
        self.request("GET", path, "")
    }

    fn request(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", self.address, body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        (
            head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            serde_json::from_str(body).unwrap(),
        )
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct MockMint {
    pub url: String,
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}

impl MockMint {
    #[allow(clippy::too_many_lines)] // One HTTP fixture allowlist covers Cashu and the bridge; unexpected writes panic.
    pub fn start(mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let directory_url = url.clone();
        let task = thread::spawn(move || {
            let mut stalled_connections = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let request = String::from_utf8(request).unwrap();
                let first = request.lines().next().unwrap();
                if mode == "fed_slow_quote"
                    && (first == "POST /v2/ln/ecashmesh-quote HTTP/1.1"
                        || first == "POST /v2/ln/ecashmesh-gateway-estimate HTTP/1.1")
                {
                    // Keep the response pending without blocking other bridge
                    // requests; dropping this fixture closes every connection.
                    stalled_connections.push(stream);
                    continue;
                }

                let content_length = request
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then_some(value.trim())
                        })
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut request_body = vec![0_u8; content_length];
                if content_length > 0 {
                    stream.read_exact(&mut request_body).unwrap();
                }
                let mut body = match first {
                    "GET /health HTTP/1.1"
                    | "POST /v2/ln/ecashmesh-quote HTTP/1.1"
                    | "POST /v2/ln/ecashmesh-gateway-estimate HTTP/1.1" => "{}",
                    "GET /v2/admin/info HTTP/1.1" => r#"{"network":"bitcoin"}"#,
                    "POST /v2/ln/list-gateways HTTP/1.1" => r#"[{"gateway_id":"fixture-gateway","fees":{"base_msat":1000,"proportional_millionths":100}}]"#,
                    "GET /v1/info HTTP/1.1" => {
                        include_str!("../../../ecashmesh-cashu/tests/fixtures/info.json")
                    }
                    "GET /v1/keysets HTTP/1.1" => {
                        include_str!("../../../ecashmesh-cashu/tests/fixtures/keysets.json")
                    }
                    "GET /v1/keys HTTP/1.1" => {
                        include_str!("../../../ecashmesh-cashu/tests/fixtures/keys.json")
                    }
                    "POST /v1/mint/quote/bolt11 HTTP/1.1" => {
                        r#"{"quote":"mint-quote-fixture","request":"lnbc1000u1qqqqqqq9kvtew","expiry":5000300}"#
                    }
                    "POST /v1/melt/quote/bolt11 HTTP/1.1" => {
                        r#"{"quote":"melt-quote-fixture","amount":100000,"fee_reserve":321,"expiry":5000200}"#
                    }
                    "GET /directory HTTP/1.1" => "[]",
                    other => panic!("Unexpected protocol operation: {other}"),
                }
                .to_owned();
                if first == "POST /v2/ln/ecashmesh-quote HTTP/1.1" {
                    use sha2::{Digest, Sha256};
                    let req: Value = serde_json::from_slice(&request_body).unwrap();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    let mut evidence = json!({
                        "schema":"ecashmesh-fedimint-quote-v2", "fedimint_version":"0.12.1",
                        "federation_id":req["federation_id"], "invoice_digest":format!("{:x}", Sha256::digest(req["invoice"].as_str().unwrap().as_bytes())),
                        "payment_hash":"11".repeat(32), "amount_msat":req["amount_sats"].as_u64().unwrap()*1000,
                        "destination_pubkey":format!("02{}", "22".repeat(32)),
                        "network":"bitcoin", "federation_fee_msat":1001, "gateway_fee_msat":1001, "destination_fee_msat":0,
                        "total_fee_msat":2002, "wallet_balance_msat":1_000_000_000, "funding_feasible":true,
                        "payable":null, "gateway_liquidity":"unknown", "selected_gateway_id":format!("02{}", "11".repeat(32)),
                        "gateway_identity_verified":true, "observed_at_unix_seconds":now, "expires_at_unix_seconds":now+30
                    });
                    if mode == "fed_insufficient" {
                        evidence["wallet_balance_msat"] = json!(0);
                    }
                    if mode == "fed_stale" {
                        evidence["expires_at_unix_seconds"] = json!(now);
                    }
                    body = evidence.to_string();
                }
                if first.contains("/directory ") {
                    body = json!([{"url":directory_url}]).to_string();
                }
                if first == "POST /v1/melt/quote/bolt11 HTTP/1.1" {
                    let mut value: Value = serde_json::from_str(&body).unwrap();
                    value["expiry"] = json!(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs()
                            + 30
                    );
                    body = value.to_string();
                }
                if first == "POST /v2/ln/ecashmesh-gateway-estimate HTTP/1.1"
                    && mode == "fed_estimate"
                {
                    use sha2::{Digest, Sha256};
                    let req: Value = serde_json::from_slice(&request_body).unwrap();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    let gateway_id = format!("02{}", "11".repeat(32));
                    body = json!({
                        "schema":"ecashmesh-fedimint-gateway-estimate-v1", "fedimint_version":"0.12.1",
                        "federation_id":req["federation_id"], "invoice_digest":format!("{:x}", Sha256::digest(req["invoice"].as_str().unwrap().as_bytes())),
                        "payment_hash":"11".repeat(32), "amount_msat":req["amount_sats"].as_u64().unwrap()*1000,
                        "network":"bitcoin", "federation_fee_msat":null, "gateway_fee_msat":7000,
                        "wallet_balance_msat":0, "funding_feasible":null, "payable":null,
                        "selected_gateway_id":gateway_id, "gateway_identity_verified":true,
                        "gateway_protocol":"lnv1", "gateway_candidates":[{
                            "gateway_id":gateway_id, "gateway_url":"https://gateway.example/v1",
                            "gateway_fee_msat":7000, "fee_base_msat":7000, "fee_ppm":0,
                            "gateway_identity_verified":true, "gateway_protocol":"lnv1"
                        }],
                        "observed_at_unix_seconds":now, "expires_at_unix_seconds":now+30
                    }).to_string();
                }
                if mode == "malformed" {
                    body = "{".into();
                }
                if mode == "disabled" && first.contains("/info") {
                    let mut info: Value = serde_json::from_str(&body).unwrap();
                    info["nuts"]["5"]["disabled"] = json!(true);
                    body = info.to_string();
                }
                let status = if mode == "unavailable"
                    || (mode == "quote_unavailable" && first.starts_with("POST /v1/"))
                {
                    503
                } else if first == "POST /v2/ln/ecashmesh-quote HTTP/1.1" && mode == "fed_unpatched"
                {
                    404
                } else if first == "POST /v2/ln/ecashmesh-quote HTTP/1.1"
                    && mode == "fed_unauthorized"
                {
                    401
                } else if first.starts_with("POST /v2/ln/")
                    && (mode.starts_with("fed_invoice_") || mode.starts_with("fed_gateway_"))
                {
                    body = json!({"error_code":mode.strip_prefix("fed_").unwrap().to_ascii_uppercase()}).to_string();
                    422
                } else if first == "POST /v2/ln/ecashmesh-quote HTTP/1.1"
                    && (mode == "fed_empty" || mode == "fed_estimate")
                {
                    body = json!({"error_code":"INSUFFICIENT_BALANCE"}).to_string();
                    422
                } else {
                    200
                };
                let age = if mode == "stale" { "Age: 600\r\n" } else { "" };
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n{age}Connection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            url,
            stop,
            task: Some(task),
        }
    }
}

impl Drop for MockMint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.task.take().unwrap().join().unwrap();
    }
}
