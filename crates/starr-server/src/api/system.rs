use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use starr_core::system::Health;
use utoipa::ToSchema;

use crate::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    #[schema(value_type = String, example = "ok")]
    status: Health,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StatusResponse {
    app: &'static str,
    version: &'static str,
    #[schema(value_type = String, example = "ok")]
    database: Health,
}

async fn db_health(state: &AppState) -> Health {
    match state.db.ping().await {
        Ok(()) => Health::Ok,
        Err(err) => {
            tracing::warn!(error = %err, "database ping failed");
            Health::Degraded
        }
    }
}

/// Liveness/readiness probe (used by the Docker healthcheck).
#[utoipa::path(get, path = "/api/v1/health", tag = "system",
    responses(
        (status = 200, body = HealthResponse),
        (status = 503, body = HealthResponse),
    ))]
pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let status = db_health(&state).await;
    let code = if status.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(HealthResponse { status }))
}

/// Application version and subsystem health.
#[utoipa::path(get, path = "/api/v1/system/status", tag = "system",
    responses((status = 200, body = StatusResponse)))]
pub async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(StatusResponse {
        app: starr_core::APP_NAME,
        version: starr_core::VERSION,
        database: db_health(&state).await,
    })
}
