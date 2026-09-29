//! Explicit local wallet setup, separate from the read-only evaluation adapter.
use super::{ApiError, AppState};
use axum::{Json, extract::State, http::HeaderMap};
use ecashmesh_fedimint::FedimintService;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct Plan {
    digest: String,
    federation_id: String,
    expires: Instant,
}

pub(super) struct SetupService {
    client: reqwest::Client,
    origins: Vec<String>,
    plans: Mutex<BTreeMap<String, Plan>>,
    joining: tokio::sync::Mutex<()>,
}

impl SetupService {
    pub fn new() -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(90))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "Cannot initialize setup transport")?,
            origins: std::env::var("ECASHMESH_WEB_ORIGIN").map_or_else(
                |_| {
                    vec![
                        "http://localhost:8081".into(),
                        "http://127.0.0.1:8081".into(),
                    ]
                },
                |v| vec![v],
            ),
            plans: Mutex::new(BTreeMap::new()),
            joining: tokio::sync::Mutex::new(()),
        }))
    }

    fn authorize(&self, headers: &HeaderMap, service: &FedimintService) -> Result<(), ApiError> {
        if service.catalog_host().is_none() {
            return Err(fail(
                "SETUP_DISABLED",
                "The API operator must set ECASHMESH_FEDIMINT_BRIDGE_URL and ECASHMESH_FEDIMINT_BRIDGE_TOKEN_FILE, then restart the API.",
            ));
        }
        let origin = headers.get("origin").and_then(|v| v.to_str().ok());
        if headers
            .get("x-ecashmesh-setup")
            .and_then(|v| v.to_str().ok())
            != Some("1")
            || !origin.is_some_and(|v| self.origins.iter().any(|allowed| v == allowed))
        {
            return Err(ApiError::payment_safety(
                "Federation setup requires the approved local web origin and setup header",
            ));
        }
        Ok(())
    }

    async fn call(
        &self,
        service: &FedimintService,
        path: &str,
        body: Value,
    ) -> Result<Value, ApiError> {
        let host = service
            .catalog_host()
            .ok_or_else(|| fail("SETUP_DISABLED", "Local setup is disabled"))?;
        let mut req = self
            .client
            .post(format!("{}{path}", host.bridge_url.trim_end_matches('/')))
            .json(&body);
        if let Some(token) = &host.token {
            req = req.bearer_auth(token);
        }
        let response = req.send().await.map_err(|_| fail("SETUP_UNCERTAIN", "The local bridge did not respond. Refresh connection status before retrying; a join may have completed."))?;
        if !response.status().is_success() {
            let message = match response.status().as_u16() {
                404 => "The local bridge does not expose this setup endpoint.",
                401 | 403 => {
                    "The local bridge rejected the backend credential. Check the API configuration."
                }
                _ => {
                    "The local bridge rejected setup. Verify the invite matches this federation and uses public secure guardian endpoints."
                }
            };
            return Err(fail("SETUP_REJECTED", message));
        }
        response
            .json()
            .await
            .map_err(|_| fail("SETUP_REJECTED", "Invalid bridge setup response"))
    }
}

fn fail(code: &'static str, message: &str) -> ApiError {
    ApiError::new(code, message)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreviewRequest {
    federation_id: String,
    invite_code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdentifyRequest {
    invite_code: String,
}

impl IdentifyRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.invite_code.len() > 4096 || !self.invite_code.starts_with("fed1") {
            return Err(fail("INVALID_INVITE", "Enter a valid fed1 invite code"));
        }
        Ok(())
    }
}

impl PreviewRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.federation_id.len() != 64
            || !self.federation_id.bytes().all(|b| b.is_ascii_hexdigit())
            || self.invite_code.len() > 4096
            || !self.invite_code.starts_with("fed1")
        {
            return Err(fail(
                "INVALID_INVITE",
                "Enter a federation ID and its valid fed1 invite code",
            ));
        }
        Ok(())
    }
    fn digest(&self) -> String {
        format!("{:x}", Sha256::digest(self.invite_code.as_bytes()))
    }
}

pub(super) async fn catalog(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let service = state.provider.fedimint_service();
    service.refresh_joined_catalog().await.map_err(|_| {
        fail(
            "SETUP_UNAVAILABLE",
            "Cannot read the local bridge catalog. Check the bridge and the API credential.",
        )
    })?;
    Ok(Json(json!({
        "enabled":service.catalog_host().is_some(),
        "host_label":service.catalog_host().map(|c| &c.label),
        "sources":service.configured().iter().map(|c| json!({"connector_id":c.id,"federation_id":c.federation_id,"label":c.label,"connected":service.is_joined_on_setup_host(c)})).collect::<Vec<_>>()
    })))
}

pub(super) async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    let service = state.provider.fedimint_service();
    state.setup.authorize(&headers, service)?;
    req.validate()?;
    ensure_capacity(service, &req.federation_id)?;
    let federation_id = req.federation_id.to_lowercase();
    let result = state
        .setup
        .call(
            service,
            "/v2/ln/ecashmesh-connect-preview",
            json!({"invite_code":req.invite_code,"federation_id":federation_id}),
        )
        .await?;
    if result["federation_id"] != federation_id {
        return Err(fail("INVALID_INVITE", "Invite federation ID mismatch"));
    }
    let mut random = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut random);
    let token = format!("{:x}", Sha256::digest(random));
    let mut plans = state.setup.plans.lock().expect("setup plan lock");
    plans.retain(|_, p| p.expires > Instant::now());
    if plans.len() >= 32 {
        return Err(fail(
            "SETUP_BUSY",
            "Too many pending confirmations; wait two minutes",
        ));
    }
    plans.insert(
        token.clone(),
        Plan {
            digest: req.digest(),
            federation_id: federation_id.clone(),
            expires: Instant::now() + Duration::from_mins(2),
        },
    );
    Ok(Json(
        json!({"confirmation_token":token,"federation_id":federation_id,"host_label":service.catalog_host().map(|c| &c.label),"expires_in_seconds":120,
        "warning":"Joining may create a new local wallet. It does not import another wallet's balance, guarantee trust, or execute a payment."}),
    ))
}

/// Parses an invite through the local bridge without joining. This
/// lets the UI create/reconcile the correct source before confirmation.
pub(super) async fn identify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IdentifyRequest>,
) -> Result<Json<Value>, ApiError> {
    let service = state.provider.fedimint_service();
    state.setup.authorize(&headers, service)?;
    req.validate()?;
    let result = state
        .setup
        .call(
            service,
            "/v2/ln/ecashmesh-connect-identify",
            json!({"invite_code":req.invite_code}),
        )
        .await?;
    let federation_id = result["federation_id"]
        .as_str()
        .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| fail("INVALID_INVITE", "Invalid bridge invite response"))?
        .to_lowercase();
    let label = service
        .configured()
        .into_iter()
        .find(|c| c.federation_id == federation_id)
        .map_or_else(
            || {
                format!(
                    "Federation {}…{}",
                    &federation_id[..7],
                    &federation_id[59..]
                )
            },
            |c| c.label,
        );
    Ok(Json(json!({
        "federation_id": federation_id,
        "label": label,
        "host_label": service.catalog_host().map(|c| &c.label),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConnectRequest {
    federation_id: String,
    invite_code: String,
    confirmation_token: String,
    confirmed: bool,
}

pub(super) async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ConnectRequest>,
) -> Result<Json<Value>, ApiError> {
    let service = state.provider.fedimint_service();
    state.setup.authorize(&headers, service)?;
    if !req.confirmed {
        return Err(fail(
            "CONFIRMATION_REQUIRED",
            "Confirm local wallet creation before connecting",
        ));
    }
    let _joining = state
        .setup
        .joining
        .try_lock()
        .map_err(|_| fail("SETUP_BUSY", "Another federation connection is in progress"))?;
    let input = PreviewRequest {
        federation_id: req.federation_id.to_lowercase(),
        invite_code: req.invite_code,
    };
    input.validate()?;
    ensure_capacity(service, &input.federation_id)?;
    let plan = state
        .setup
        .plans
        .lock()
        .expect("setup plan lock")
        .remove(&req.confirmation_token)
        .ok_or_else(|| {
            fail(
                "CONFIRMATION_REQUIRED",
                "Preview the invite again; confirmation is missing or already used",
            )
        })?;
    if plan.expires <= Instant::now()
        || plan.digest != input.digest()
        || plan.federation_id != input.federation_id
    {
        return Err(fail(
            "CONFIRMATION_REQUIRED",
            "Invite changed or confirmation expired; preview again",
        ));
    }
    let result = state.setup.call(service, "/v2/ln/ecashmesh-connect", json!({"federation_id":input.federation_id,"invite_code":input.invite_code,"confirmed":true})).await?;
    if result["federation_id"] != input.federation_id {
        return Err(fail(
            "SETUP_UNCERTAIN",
            "Unexpected joined federation ID. Refresh local status.",
        ));
    }
    service.refresh_joined_catalog().await.map_err(|_| fail("SETUP_UNCERTAIN", "The bridge joined, but catalog refresh failed. Refresh connection status; do not create another wallet."))?;
    let config = service
        .configured()
        .into_iter()
        .find(|c| c.federation_id == input.federation_id)
        .ok_or_else(|| {
            fail(
                "SETUP_UNCERTAIN",
                "Joined federation not yet visible; refresh connection status",
            )
        })?;
    Ok(Json(
        json!({"connector_id":config.id,"federation_id":config.federation_id,"label":config.label,"status":"connected","funded":null,"wallet_executable":false}),
    ))
}

fn ensure_capacity(service: &FedimintService, federation_id: &str) -> Result<(), ApiError> {
    let configs = service.configured();
    if let Some(existing) = configs
        .iter()
        .find(|c| c.federation_id.eq_ignore_ascii_case(federation_id))
    {
        if service
            .catalog_host()
            .is_some_and(|host| host.bridge_url != existing.bridge_url)
        {
            return Err(fail(
                "SETUP_REJECTED",
                "This federation is configured on another local bridge. Use that bridge.",
            ));
        }
    } else if configs.len() >= 32 {
        return Err(fail(
            "SETUP_REJECTED",
            "Federation catalog limit reached (32); no join was attempted",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        routing::{get, post},
    };
    use ecashmesh_fedimint::{FederationConfig, QuoteBackend};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Clone)]
    struct MockDaemon {
        joined: Arc<AtomicBool>,
        joins: Arc<AtomicUsize>,
    }

    async fn fixture(enabled: bool) -> (AppState, MockDaemon, tokio::task::JoinHandle<()>) {
        let mock = MockDaemon {
            joined: Arc::new(AtomicBool::new(false)),
            joins: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route(
                "/v2/admin/info",
                get(|State(m): State<MockDaemon>| async move {
                    let mut v = json!({});
                    v["11".repeat(32)] = json!({"meta":{"federation_name":"Host"}});
                    if m.joined.load(Ordering::SeqCst) {
                        v["22".repeat(32)] = json!({"meta":{"federation_name":"Joined"}});
                    }
                    Json(v)
                }),
            )
            .route(
                "/v2/ln/ecashmesh-connect-preview",
                post(|headers: HeaderMap, Json(v): Json<Value>| async move {
                    assert_eq!(headers["authorization"], "Bearer synthetic-backend-secret");
                    Json(json!({"federation_id":v["federation_id"]}))
                }),
            )
            .route(
                "/v2/ln/ecashmesh-connect",
                post(
                    |State(m): State<MockDaemon>, Json(v): Json<Value>| async move {
                        assert_eq!(v["confirmed"], true);
                        m.joins.fetch_add(1, Ordering::SeqCst);
                        m.joined.store(true, Ordering::SeqCst);
                        Json(json!({"federation_id":v["federation_id"]}))
                    },
                ),
            )
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = FederationConfig {
            id: "fedimint:host".into(),
            label: "Host".into(),
            federation_id: "11".repeat(32),
            bridge_url: url,
            token: Some("synthetic-backend-secret".into()),
            quote_backend: QuoteBackend::LocalV0121Bridge,
        };
        let service = FedimintService::new(vec![config], 300)
            .unwrap()
            .with_catalog_host(enabled.then_some("fedimint:host"))
            .unwrap();
        let mut setup = SetupService::new().unwrap();
        Arc::get_mut(&mut setup).unwrap().origins = vec!["http://localhost:8081".into()];
        (
            AppState {
                provider: super::super::connectors::Provider::for_setup_test(service),
                payments: super::super::payment::PaymentService::from_env().unwrap(),
                setup,
            },
            mock,
            task,
        )
    }
    fn headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("origin", "http://localhost:8081".parse().unwrap());
        h.insert("x-ecashmesh-setup", "1".parse().unwrap());
        h
    }
    fn input() -> PreviewRequest {
        PreviewRequest {
            federation_id: "22".repeat(32),
            invite_code: "fed1-synthetic-invite".into(),
        }
    }
    fn confirmation(token: &str) -> ConnectRequest {
        ConnectRequest {
            federation_id: input().federation_id,
            invite_code: input().invite_code,
            confirmation_token: token.into(),
            confirmed: true,
        }
    }

    #[tokio::test]
    async fn preview_never_joins_confirmation_is_bound_and_catalog_recovers_after_restart() {
        let (state, mock, task) = fixture(true).await;
        let result = preview(State(state.clone()), headers(), Json(input()))
            .await
            .unwrap()
            .0;
        assert_eq!(mock.joins.load(Ordering::SeqCst), 0);
        let serialized = result.to_string();
        assert!(!serialized.contains("synthetic-invite") && !serialized.contains("backend-secret"));
        let token = result["confirmation_token"].as_str().unwrap();
        let mut unconfirmed = confirmation(token);
        unconfirmed.confirmed = false;
        assert!(
            connect(State(state.clone()), headers(), Json(unconfirmed))
                .await
                .is_err()
        );
        let mut altered = confirmation(token);
        altered.invite_code = "fed1-changed".into();
        assert!(
            connect(State(state.clone()), headers(), Json(altered))
                .await
                .is_err()
        );
        assert_eq!(mock.joins.load(Ordering::SeqCst), 0);
        let result = preview(State(state.clone()), headers(), Json(input()))
            .await
            .unwrap()
            .0;
        let token = result["confirmation_token"].as_str().unwrap();
        let connected = connect(State(state.clone()), headers(), Json(confirmation(token)))
            .await
            .unwrap()
            .0;
        assert_eq!(
            connected["connector_id"],
            format!("fedimint:{}", "22".repeat(32))
        );
        assert_eq!(connected["wallet_executable"], false);
        assert!(
            connect(State(state.clone()), headers(), Json(confirmation(token)))
                .await
                .is_err()
        );
        assert_eq!(mock.joins.load(Ordering::SeqCst), 1);
        let host = state
            .provider
            .fedimint_service()
            .catalog_host()
            .unwrap()
            .clone();
        let restarted = FedimintService::new(vec![host], 300)
            .unwrap()
            .with_catalog_host(Some("fedimint:host"))
            .unwrap();
        restarted.refresh_joined_catalog().await.unwrap();
        assert!(
            restarted
                .configured()
                .iter()
                .all(|config| config.id != "fedimint:host")
        );
        let joined = restarted
            .configured()
            .into_iter()
            .find(|c| c.federation_id == "22".repeat(32))
            .unwrap();
        assert!(restarted.is_joined_on_setup_host(&joined));
        let catalog = catalog(State(state)).await.unwrap().0.to_string();
        assert!(!catalog.contains("backend-secret") && !catalog.contains("bridge_url"));
        task.abort();
    }

    #[tokio::test]
    async fn disabled_unapproved_origin_missing_header_and_expired_confirmation_never_join() {
        let (state, mock, task) = fixture(false).await;
        assert!(
            preview(State(state), headers(), Json(input()))
                .await
                .is_err()
        );
        assert_eq!(mock.joins.load(Ordering::SeqCst), 0);
        task.abort();
        let (state, mock, task) = fixture(true).await;
        let mut evil = headers();
        evil.insert("origin", "https://attacker.example".parse().unwrap());
        assert!(
            preview(State(state.clone()), evil, Json(input()))
                .await
                .is_err()
        );
        let mut missing = headers();
        missing.remove("x-ecashmesh-setup");
        assert!(
            preview(State(state.clone()), missing, Json(input()))
                .await
                .is_err()
        );
        let result = preview(State(state.clone()), headers(), Json(input()))
            .await
            .unwrap()
            .0;
        let token = result["confirmation_token"].as_str().unwrap();
        state
            .setup
            .plans
            .lock()
            .unwrap()
            .get_mut(token)
            .unwrap()
            .expires = Instant::now();
        assert!(
            connect(State(state.clone()), headers(), Json(confirmation(token)))
                .await
                .is_err()
        );
        assert_eq!(mock.joins.load(Ordering::SeqCst), 0);
        task.abort();
    }
}
