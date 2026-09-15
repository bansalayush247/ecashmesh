//! Reference-host confirmation transport. No payment is executed or persisted.

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
};
use serde::{Deserialize, Serialize};

use super::{ApiError, EvaluateRequest, FeeResponse, Provider, deterministic_id, evaluate_using};

#[derive(Deserialize)]
pub(super) struct SimulationRequest {
    payment: EvaluateRequest,
    quote_id: String,
    route_id: String,
}

#[derive(Serialize)]
pub(super) struct SimulationReceipt {
    simulation_id: String,
    status: &'static str,
    simulated: bool,
    quote_id: String,
    route_id: String,
    amount: u64,
    asset: &'static str,
    fee: FeeResponse,
    path: Vec<String>,
    message: &'static str,
}

pub(super) async fn confirm(
    State(provider): State<Provider>,
    request: Result<Json<SimulationRequest>, JsonRejection>,
) -> Result<Json<SimulationReceipt>, ApiError> {
    let Json(request) = request.map_err(|error| {
        ApiError::validation("Invalid simulator confirmation", vec![error.to_string()])
    })?;
    let amount = request.payment.amount;
    if !matches!(provider, Provider::Simulator) {
        request.payment.validate()?;
        if request.quote_id.trim().is_empty() || request.route_id.trim().is_empty() {
            return Err(ApiError::validation(
                "Live simulator completion needs a quote and selected route",
                vec!["quote_id".into(), "route_id".into()],
            ));
        }
        return Ok(Json(SimulationReceipt {
            simulation_id: deterministic_id(
                "live_simulation",
                &[request.quote_id.clone(), request.route_id.clone()],
            ),
            status: "simulated_success",
            simulated: true,
            quote_id: request.quote_id,
            route_id: request.route_id,
            amount,
            asset: "BTC",
            fee: FeeResponse {
                amount: None,
                asset: "sats",
                freshness: "unknown",
            },
            path: Vec::new(),
            message: "Live route evaluation was confirmed in the host simulator. No funds moved.",
        }));
    }
    // Reuse the same engine/fixture inputs. The host cannot supply a fabricated
    // route, fee, score, or success result. Alternative selections are allowed.
    let Json(decision) = evaluate_using(&provider, Ok(Json(request.payment))).await?;
    if request.quote_id != decision.quote_id {
        return Err(ApiError::validation(
            "Quote does not match this simulated payment; evaluate again.",
            vec!["quote_id".into()],
        ));
    }
    let route = std::iter::once(decision.recommended_route)
        .chain(decision.alternatives)
        .find(|route| route.route_id == request.route_id)
        .ok_or_else(|| {
            ApiError::validation(
                "Selected route is not in this evaluation; evaluate again.",
                vec!["route_id".into()],
            )
        })?;
    Ok(Json(SimulationReceipt {
        simulation_id: deterministic_id(
            "simulation",
            &[request.quote_id.clone(), request.route_id.clone()],
        ),
        status: "simulated_success",
        simulated: true,
        quote_id: request.quote_id,
        route_id: request.route_id,
        amount,
        asset: "BTC",
        fee: route.fee,
        path: route.path,
        message: "Simulator completed the selected route. No funds moved.",
    }))
}
