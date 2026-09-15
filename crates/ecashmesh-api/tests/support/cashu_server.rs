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
    pub fn start(mint_url: &str) -> Self {
        Self::start_with_sources(
            &json!([{"id":"cashu:fixture","url":mint_url}]),
            &json!([]),
            &json!([]),
        )
    }

    pub fn start_with_sources(seeds: &Value, directories: &Value, allowed: &Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_ecashmesh-api"))
            .env("ECASHMESH_API_ADDRESS", &address)
            .env("ECASHMESH_CONNECTOR_MODE", "cashu")
            .env("ECASHMESH_CASHU_MAX_AGE_SECONDS", "300")
            .env("ECASHMESH_CASHU_MINTS", seeds.to_string())
            .env("ECASHMESH_CASHU_DIRECTORIES", directories.to_string())
            .env("ECASHMESH_CASHU_ALLOWED_MINTS", allowed.to_string())
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
    pub fn start(mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let directory_url = url.clone();
        let task = thread::spawn(move || {
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
                if content_length > 0 {
                    let mut request_body = vec![0_u8; content_length];
                    stream.read_exact(&mut request_body).unwrap();
                }
                let mut body = match first {
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
                if first.contains("/directory ") {
                    body = json!([{"url":directory_url}]).to_string();
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
