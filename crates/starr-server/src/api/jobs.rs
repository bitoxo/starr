//! Manual job triggers: `/api/v1/jobs`. The web side only enqueues and wakes
//! the worker; it never executes jobs (ADR 0003, 0004).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use serde::Serialize;
use starr_core::jobs::{JobKind, Trigger};
use starr_db::jobs::NewJob;
use utoipa::ToSchema;

use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::AdminSession;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriggeredJob {
    /// The queued job. If the same kind was already pending, this is that job
    /// (pulled forward to now); if it is running, this is the running job and
    /// no further execution is queued.
    job_id: i64,
}

/// Run a job now. Every job kind can be triggered by hand.
#[utoipa::path(post, path = "/api/v1/jobs/{kind}/trigger", tag = "jobs",
    params(("kind" = String, Path, description = "Job kind, e.g. `system.housekeeping`")),
    responses(
        (status = 202, body = TriggeredJob),
        (status = 404, body = ErrorBody, description = "`jobs.unknown_kind`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn trigger_job(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> Result<(StatusCode, Json<TriggeredJob>), ApiError> {
    let Some(kind) = JobKind::from_name(&kind) else {
        return Err(ApiError::NotFound("jobs.unknown_kind"));
    };
    let now = Utc::now();
    let key = kind.unique_key();
    let job_id = state
        .db
        .enqueue_job(
            &NewJob {
                kind: kind.name(),
                payload: "{}",
                unique_key: Some(&key),
                trigger: Trigger::Manual,
                run_after: now,
                max_attempts: kind.spec().max_attempts,
            },
            now,
        )
        .await?;
    state.wake.notify_one();
    Ok((StatusCode::ACCEPTED, Json(TriggeredJob { job_id })))
}
