//! Run history: `/api/v1/runs`.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use starr_core::jobs::{RunState, Trigger};
use starr_db::DbError;
use starr_db::runs::{Run, RunFilter, RunOutcome, RunTransition};
use utoipa::{IntoParams, ToSchema};

use super::error::{ApiError, ErrorBody};
use crate::AppState;
use crate::auth::AdminSession;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = RunState)]
pub enum RunStateDto {
    Running,
    Succeeded,
    Failed,
    Aborted,
}

impl From<RunState> for RunStateDto {
    fn from(state: RunState) -> Self {
        match state {
            RunState::Running => Self::Running,
            RunState::Succeeded => Self::Succeeded,
            RunState::Failed => Self::Failed,
            RunState::Aborted => Self::Aborted,
        }
    }
}

impl From<RunStateDto> for RunState {
    fn from(state: RunStateDto) -> Self {
        match state {
            RunStateDto::Running => Self::Running,
            RunStateDto::Succeeded => Self::Succeeded,
            RunStateDto::Failed => Self::Failed,
            RunStateDto::Aborted => Self::Aborted,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = Trigger)]
pub enum TriggerDto {
    Schedule,
    Manual,
}

impl From<Trigger> for TriggerDto {
    fn from(trigger: Trigger) -> Self {
        match trigger {
            Trigger::Schedule => Self::Schedule,
            Trigger::Manual => Self::Manual,
        }
    }
}

/// Whether the run had the effect it should have had.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = Expectation)]
pub enum ExpectationDto {
    Met,
    Unmet,
    NotApplicable,
}

impl ExpectationDto {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "met" => Some(Self::Met),
            "unmet" => Some(Self::Unmet),
            "not_applicable" => Some(Self::NotApplicable),
            _ => None,
        }
    }
}

/// One execution of a background job.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    id: i64,
    job_id: i64,
    /// Job kind, e.g. `system.housekeeping`.
    kind: String,
    trigger: TriggerDto,
    /// 1 for the first attempt.
    attempt: u32,
    state: RunStateDto,
    /// 0.0..=1.0 while running, if the job reports progress.
    progress: Option<f64>,
    /// Set when the run produced a report.
    expectation: Option<ExpectationDto>,
    error: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl From<&Run> for RunSummary {
    fn from(run: &Run) -> Self {
        Self {
            id: run.id,
            job_id: run.job_id,
            kind: run.kind.clone(),
            trigger: run.trigger.into(),
            attempt: run.attempt,
            state: run.state.into(),
            progress: run.progress,
            expectation: run.expectation.as_deref().and_then(ExpectationDto::parse),
            error: run.error.clone(),
            started_at: run.started_at,
            finished_at: run.finished_at,
        }
    }
}

/// One page of runs, newest first.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunPage {
    runs: Vec<RunSummary>,
    /// Pass as `before` to get the next page; absent on the last page.
    next_before: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = RunTransition)]
pub struct RunTransitionDto {
    /// Absent for the first transition (run created).
    from: Option<RunStateDto>,
    to: RunStateDto,
    note: Option<String>,
    at: DateTime<Utc>,
}

impl From<RunTransition> for RunTransitionDto {
    fn from(t: RunTransition) -> Self {
        Self {
            from: t.from.map(Into::into),
            to: t.to.into(),
            note: t.note,
            at: t.at,
        }
    }
}

/// Expectation check of a report.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ExpectationCheck {
    status: ExpectationDto,
    /// What was expected and what happened.
    detail: Option<String>,
}

/// Outcome of one unit of work in a report.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = ItemOutcome)]
pub enum ItemOutcomeDto {
    Succeeded,
    Failed,
    Skipped,
}

/// One unit of work in a report, e.g. one service instance checked.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunReportItem {
    target: String,
    outcome: ItemOutcomeDto,
    /// Machine-readable code (error taxonomy), translated by the UI.
    code: Option<String>,
    /// Detail for operators, untranslated.
    detail: Option<String>,
}

/// Structured result of a successful run. `succeeded + failed + skipped`
/// always equals `total`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    total: u64,
    succeeded: u64,
    failed: u64,
    skipped: u64,
    /// Job-specific counters, e.g. `runs_deleted`.
    counters: BTreeMap<String, u64>,
    expectation: ExpectationCheck,
    /// Per-unit outcomes, for jobs with distinct units.
    #[serde(default)]
    items: Vec<RunReportItem>,
}

/// A run with its report and state history.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    #[serde(flatten)]
    run: RunSummary,
    /// Present when the run succeeded.
    report: Option<RunReport>,
    transitions: Vec<RunTransitionDto>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct RunQuery {
    /// Page size (1–200, default 50).
    limit: Option<u32>,
    /// Only runs with an id below this (from `nextBefore`).
    before: Option<i64>,
    /// Only runs of this job kind.
    kind: Option<String>,
    /// Only runs in this state.
    state: Option<RunStateDto>,
    /// Only runs of this job (e.g. the `jobId` returned when it was queued).
    job_id: Option<i64>,
    /// Only runs that turned out this way.
    #[param(inline)]
    outcome: Option<RunOutcomeDto>,
}

/// How a run turned out: `attention` is a succeeded run whose expectation
/// was not met; `failed` includes aborted runs.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = RunOutcome)]
pub enum RunOutcomeDto {
    Running,
    Succeeded,
    Attention,
    Failed,
}

impl From<RunOutcomeDto> for RunOutcome {
    fn from(outcome: RunOutcomeDto) -> Self {
        match outcome {
            RunOutcomeDto::Running => Self::Running,
            RunOutcomeDto::Succeeded => Self::Succeeded,
            RunOutcomeDto::Attention => Self::Attention,
            RunOutcomeDto::Failed => Self::Failed,
        }
    }
}

/// List runs, newest first.
#[utoipa::path(get, path = "/api/v1/runs", tag = "runs",
    params(RunQuery),
    responses(
        (status = 200, body = RunPage),
        (status = 400, description = "Invalid query parameter"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn list_runs(
    _admin: AdminSession,
    State(state): State<AppState>,
    Query(query): Query<RunQuery>,
) -> Result<Json<RunPage>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let runs = state
        .db
        .list_runs(RunFilter {
            kind: query.kind.as_deref(),
            state: query.state.map(Into::into),
            job_id: query.job_id,
            outcome: query.outcome.map(Into::into),
            before: query.before,
            limit,
        })
        .await?;
    let next_before = if runs.len() == limit as usize {
        runs.last().map(|r| r.id)
    } else {
        None
    };
    Ok(Json(RunPage {
        runs: runs.iter().map(RunSummary::from).collect(),
        next_before,
    }))
}

/// One run with report and transitions.
#[utoipa::path(get, path = "/api/v1/runs/{id}", tag = "runs",
    params(("id" = i64, Path, description = "Run id")),
    responses(
        (status = 200, body = RunDetail),
        (status = 404, body = ErrorBody, description = "`runs.not_found`"),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn get_run(
    _admin: AdminSession,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RunDetail>, ApiError> {
    let Some((run, transitions)) = state.db.get_run(id).await? else {
        return Err(ApiError::NotFound("runs.not_found"));
    };
    let report = run
        .report
        .clone()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| ApiError::from(DbError::Json(err)))?;
    Ok(Json(RunDetail {
        run: RunSummary::from(&run),
        report,
        transitions: transitions.into_iter().map(Into::into).collect(),
    }))
}
