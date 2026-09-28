//! Explicit wallet setup extension, separate from read-only fee evaluation.
use crate::{error::AppError, state::AppState};
use anyhow::{anyhow, ensure, Context};
use axum::{extract::State, http::StatusCode, Json};
use multimint::fedimint_core::config::FederationId;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::instrument::WithSubscriber;

// Wallet setup is separate from quote(). These new endpoints are protected by
// clientd's existing bearer middleware and require explicit API confirmation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectPreviewRequest {
    federation_id: FederationId,
    invite_code: String,
}

fn checked_invite(
    req: &ConnectPreviewRequest,
) -> anyhow::Result<multimint::fedimint_core::invite_code::InviteCode> {
    use multimint::fedimint_core::invite_code::InviteCode;
    ensure!(req.invite_code.len() <= 4096, "Invite too long");
    let invite: InviteCode = req.invite_code.parse().context("Invalid invite")?;
    ensure!(
        invite.federation_id() == req.federation_id,
        "Invite federation ID mismatch"
    );
    for peer in invite.peers().values() {
        // SafeUrl's Display intentionally hides query/credentials. Validate
        // its actual URL, not a redacted display string.
        let url: url::Url = peer.as_str().parse()?;
        ensure!(
            url.scheme() == "wss"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Secure public guardian URLs required"
        );
        let host = url.host_str().context("Missing guardian host")?;
        ensure!(
            host != "localhost" && !host.ends_with(".localhost") && !host.ends_with(".local"),
            "Local guardian is not allowed"
        );
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
            ensure!(public_guardian(ip), "Private guardian is not allowed");
        }
    }
    Ok(invite)
}

fn public_guardian(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (b == 18 || b == 19))
        }
        std::net::IpAddr::V6(ip) => {
            let segments = ip.segments();
            segments[0] & 0xe000 == 0x2000 && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

pub async fn connect_preview(
    Json(req): Json<ConnectPreviewRequest>,
) -> Result<Json<Value>, AppError> {
    checked_invite(&req).map_err(|_| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            anyhow!("Invite does not match federation or public guardian policy"),
        )
    })?;
    Ok(Json(json!({"federation_id":req.federation_id})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectRequest {
    federation_id: FederationId,
    invite_code: String,
    confirmed: bool,
}

static CONNECT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn connect_federation(
    State(state): State<AppState>,
    Json(req): Json<ConnectRequest>,
) -> Result<Json<Value>, AppError> {
    if !req.confirmed {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            anyhow!("Confirmation required"),
        ));
    }
    let _guard = CONNECT_LOCK.try_lock().map_err(|_| {
        AppError::new(
            StatusCode::CONFLICT,
            anyhow!("Another connection is in progress"),
        )
    })?;
    let invite = checked_invite(&ConnectPreviewRequest {
        federation_id: req.federation_id,
        invite_code: req.invite_code,
    })
    .map_err(|_| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            anyhow!("Invite does not match federation or public guardian policy"),
        )
    })?;
    if !state.multimint.has(&req.federation_id).await {
        let join = async {
            for peer in invite.peers().values() {
                let url: url::Url = peer.as_str().parse()?;
                let host = url.host_str().context("Missing guardian host")?;
                let addresses: Vec<_> =
                    tokio::net::lookup_host((host, url.port_or_known_default().unwrap_or(443)))
                        .await?
                        .collect();
                ensure!(
                    !addresses.is_empty() && addresses.iter().all(|a| public_guardian(a.ip())),
                    "Guardian resolves to private address"
                );
            }
            let mut multimint = state.multimint.clone();
            let id = multimint.register_new(invite).await?;
            ensure!(id == req.federation_id, "Joined ID mismatch");
            Ok::<_, anyhow::Error>(())
        };
        // Suppress upstream debug logs: invites may include federation secrets.
        tokio::time::timeout(
            Duration::from_secs(75),
            join.with_subscriber(tracing::subscriber::NoSubscriber::default()),
        )
        .await
        .map_err(|_| {
            AppError::new(
                StatusCode::GATEWAY_TIMEOUT,
                anyhow!("Join timed out; inspect joined catalog before retrying"),
            )
        })?
        .map_err(|_| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                anyhow!("Federation join failed; inspect joined catalog before retrying"),
            )
        })?;
    }
    Ok(Json(
        json!({"federation_id":req.federation_id,"status":"connected"}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    const INVITE: &str = "fed11qgqzxgthwden5te0v9cxjtnzd96xxmmfdckhqunfde3kjurvv4ejucm0d5hsqqfqkggx3jz0tvfv5n7lj0e7gs7nh47z06ry95x4963wfh8xlka7a80su3952t";
    const ID: &str = "b21068c84f5b12ca4fdf93f3e443d3bd7c27e8642d0d52ea2e4dce6fdbbee9df";

    #[test]
    fn pinned_native_invite_parser_binds_identity_without_network_or_join() {
        let req = ConnectPreviewRequest {
            federation_id: ID.parse().unwrap(),
            invite_code: INVITE.into(),
        };
        assert_eq!(
            checked_invite(&req).unwrap().federation_id().to_string(),
            ID
        );
        let wrong = ConnectPreviewRequest {
            federation_id: "11".repeat(32).parse().unwrap(),
            invite_code: INVITE.into(),
        };
        assert!(checked_invite(&wrong).is_err());
        assert!(checked_invite(&ConnectPreviewRequest {
            federation_id: req.federation_id,
            invite_code: "fed1-invalid".into()
        })
        .is_err());
    }

    #[test]
    fn rejects_local_or_insecure_guardian_invites() {
        use multimint::fedimint_core::{invite_code::InviteCode, PeerId};
        for url in [
            "ws://guardian.example",
            "wss://localhost",
            "wss://127.0.0.1",
            "wss://10.0.0.1",
            "wss://[::1]",
            "wss://guardian.example/?secret=value",
            "wss://user:password@guardian.example/",
            "wss://guardian.example/#fragment",
        ] {
            let invite = InviteCode::new(
                url.parse().unwrap(),
                PeerId::from(0_u16),
                ID.parse().unwrap(),
                None,
            );
            assert!(
                checked_invite(&ConnectPreviewRequest {
                    federation_id: ID.parse().unwrap(),
                    invite_code: invite.to_string()
                })
                .is_err(),
                "accepted {url}"
            );
        }
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
        ] {
            assert!(!public_guardian(ip.parse().unwrap()));
        }
    }
}
