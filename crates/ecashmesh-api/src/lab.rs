//! Read-only API surface for the isolated local interoperability lab.
//!
//! The lab runner owns execution and writes the result artifact.  This module
//! intentionally never starts a payment, discovers sources, or changes normal
//! route evaluation.

use std::{env, fs, path::PathBuf};

use axum::{Json, extract::State, http::StatusCode};
use serde_json::Value;

use crate::AppState;

const RESULTS_SCHEMA: &str = "ecashmesh-lab-results-v1";

#[derive(Clone)]
pub(super) struct LabResults {
    result_file: PathBuf,
}

impl LabResults {
    pub(super) fn from_env() -> Result<Option<Self>, String> {
        let enabled = env::var("ECASHMESH_LAB_MODE").unwrap_or_default();
        if enabled.is_empty() || enabled == "false" {
            return Ok(None);
        }
        if enabled != "true" {
            return Err("ECASHMESH_LAB_MODE must be true or false".into());
        }
        if env::var("PAYMENT_ENVIRONMENT").as_deref() != Ok("regtest") {
            return Err("lab results require PAYMENT_ENVIRONMENT=regtest".into());
        }
        let root = env::current_dir()
            .map_err(|error| format!("reading lab root: {error}"))?
            .join(".regtest/ecashmesh-lab");
        let result_file = env::var_os("ECASHMESH_LAB_RESULTS_FILE")
            .map_or_else(|| root.join("results.json"), PathBuf::from);
        if !result_file.is_absolute() {
            return Err("ECASHMESH_LAB_RESULTS_FILE must be absolute".into());
        }
        if !result_file.starts_with(&root) {
            return Err("lab result file must stay under .regtest/ecashmesh-lab".into());
        }
        Ok(Some(Self { result_file }))
    }

    fn latest(&self) -> Result<Value, LabError> {
        let bytes = fs::read(&self.result_file).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => LabError::NotRun,
            _ => LabError::Unavailable,
        })?;
        let result: Value = serde_json::from_slice(&bytes).map_err(|_| LabError::Invalid)?;
        validate(&result)?;
        Ok(result)
    }
}

pub(super) async fn latest(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(results) = &state.lab_results else {
        return Err(error(StatusCode::NOT_FOUND, "LAB_MODE_DISABLED"));
    };
    results.latest().map(Json).map_err(|reason| match reason {
        LabError::NotRun => error(StatusCode::NOT_FOUND, "LAB_RESULTS_NOT_FOUND"),
        LabError::Unavailable => error(StatusCode::SERVICE_UNAVAILABLE, "LAB_RESULTS_UNAVAILABLE"),
        LabError::Invalid => error(StatusCode::UNPROCESSABLE_ENTITY, "LAB_RESULTS_INVALID"),
    })
}

#[derive(Debug, Clone, Copy)]
enum LabError {
    NotRun,
    Unavailable,
    Invalid,
}

fn error(status: StatusCode, code: &'static str) -> (StatusCode, Json<Value>) {
    (status, Json(serde_json::json!({ "error_code": code })))
}

fn validate(result: &Value) -> Result<(), LabError> {
    let object = result.as_object().ok_or(LabError::Invalid)?;
    if object.get("schema").and_then(Value::as_str) != Some(RESULTS_SCHEMA)
        || object.get("format_version").and_then(Value::as_u64) != Some(1)
        || object.get("run_id").and_then(Value::as_str).is_none()
        || object.get("timestamp").and_then(Value::as_u64).is_none()
        || object
            .get("timestamp_unix_seconds")
            .and_then(Value::as_u64)
            .is_none()
        || result.pointer("/topology/network").and_then(Value::as_str) != Some("regtest")
    {
        return Err(LabError::Invalid);
    }
    let sources = result
        .pointer("/topology/sources")
        .and_then(Value::as_array)
        .ok_or(LabError::Invalid)?;
    if sources.len() != 8 {
        return Err(LabError::Invalid);
    }
    let routes = object
        .get("routes")
        .and_then(Value::as_array)
        .ok_or(LabError::Invalid)?;
    if routes.len() != 56 {
        return Err(LabError::Invalid);
    }
    let summary = object
        .get("summary")
        .and_then(Value::as_object)
        .ok_or(LabError::Invalid)?;
    let total = summary
        .get("total")
        .and_then(Value::as_u64)
        .ok_or(LabError::Invalid)?;
    let succeeded = summary
        .get("succeeded")
        .and_then(Value::as_u64)
        .ok_or(LabError::Invalid)?;
    let failed = summary
        .get("failed")
        .and_then(Value::as_u64)
        .ok_or(LabError::Invalid)?;
    if total != routes.len() as u64 || succeeded + failed != total {
        return Err(LabError::Invalid);
    }
    if contains_secret_field(result) {
        return Err(LabError::Invalid);
    }
    for route in routes {
        let status = route
            .get("status")
            .and_then(Value::as_str)
            .ok_or(LabError::Invalid)?;
        if !matches!(status, "PASS" | "FAIL" | "NOT_RUN") {
            return Err(LabError::Invalid);
        }
        if status == "PASS"
            && (route.pointer("/settlement/status").and_then(Value::as_str) != Some("success")
                || route
                    .pointer("/settlement/destination_verified")
                    .and_then(Value::as_bool)
                    != Some(true)
                || route
                    .pointer("/settlement/latency_ms")
                    .and_then(Value::as_u64)
                    .is_none())
        {
            return Err(LabError::Invalid);
        }
    }
    Ok(())
}

fn contains_secret_field(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            matches!(
                key.as_str(),
                "mnemonic" | "macaroon" | "tls_cert" | "tls_certificate" | "private_key" | "seed"
            ) || contains_secret_field(value)
        }),
        Value::Array(values) => values.iter().any(contains_secret_field),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate;

    fn result(status: &str, verified: bool) -> serde_json::Value {
        let route = json!({
            "source":"cashu:A", "destination":"cashu:B", "status":status,
            "settlement":{"status":"success", "destination_verified":verified, "latency_ms":1}
        });
        json!({
            "schema":"ecashmesh-lab-results-v1", "format_version":1, "run_id":"test", "timestamp":1, "timestamp_unix_seconds":1,
            "topology":{"network":"regtest", "sources":vec!["source"; 8]},
            "routes":vec![route; 56],
            "summary":{"total":56,"succeeded":56,"failed":0}
        })
    }

    #[test]
    fn pass_requires_destination_side_confirmation() {
        assert!(validate(&result("PASS", true)).is_ok());
        assert!(validate(&result("PASS", false)).is_err());
    }

    #[test]
    fn rejects_artifacts_that_contain_wallet_secrets() {
        let mut value = result("PASS", true);
        value["routes"][0]["macaroon"] = json!("secret");
        assert!(validate(&value).is_err());
    }
}
