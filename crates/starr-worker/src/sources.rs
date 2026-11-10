//! `sources.lists`: read the quality profiles, root folders and (Lidarr)
//! metadata profiles of one source instance for the category settings
//! (spec 0003 D9, spec 0006 Q6) and store them, or the error, in `source_lists`. Service credentials
//! are opened here, in the worker (ADR 0003).

use starr_core::jobs::{Expectation, ItemOutcome, Report, ReportItem};

use crate::services::{CODE_ADAPTER_UNKNOWN, Failure, Openers, instance_client};
use crate::{JobContext, JobError, adapters};

pub(crate) async fn lists(ctx: &JobContext, openers: Openers<'_>) -> Result<Report, JobError> {
    let id = ctx
        .payload
        .get("instanceId")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| JobError::Failed("instanceId must be an integer".to_owned()))?;
    let Some(conn) = ctx.db.service_connection(id).await? else {
        return Ok(
            Report::new(0, 0, 0, 1, Expectation::NotApplicable)?.with_item(ReportItem {
                target: format!("service {id}"),
                outcome: ItemOutcome::Skipped,
                code: None,
                detail: Some("removed before the job ran".to_owned()),
            }),
        );
    };
    let target = format!("{} ({})", conn.name, conn.adapter_type);
    let read = async {
        let (_, client) = instance_client(openers, &conn)?;
        adapters::source_lists(&conn.adapter_type, client)
            .await
            .ok_or_else(|| {
                Failure::starr(
                    CODE_ADAPTER_UNKNOWN,
                    format!("{} is not a source", conn.adapter_type),
                )
            })?
            .map_err(Failure::from)
    };
    let result = tokio::select! {
        result = read => result,
        () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
    };
    let now = chrono::Utc::now();
    match result {
        Ok(adapters::SourceLists {
            quality_profiles: profiles,
            root_folders: folders,
            metadata_profiles: metadata,
        }) => {
            // Settings the lists no longer offer are cleared (spec 0008
            // ADM-01): the address now leads to another server.
            let cleared = ctx
                .db
                .store_source_listing(
                    id,
                    &profiles.items,
                    &folders.items,
                    &metadata.items,
                    profiles.complete && folders.complete && metadata.complete,
                    now,
                )
                .await?;
            let metadata_detail = if metadata.items.is_empty() {
                String::new()
            } else {
                format!(", {} metadata profiles", metadata.items.len())
            };
            // Counted, not named: a run report never names the private category.
            let cleared_detail = match cleared.len() {
                0 => String::new(),
                n => format!("; settings of {n} categories cleared (not offered here)"),
            };
            let detail = format!(
                "{} quality profiles, {} root folders{metadata_detail}{cleared_detail}",
                profiles.items.len(),
                folders.items.len()
            );
            Ok(
                Report::new(1, 1, 0, 0, Expectation::Met("lists read".to_owned()))?
                    .with_counter("quality_profiles", profiles.items.len() as u64)
                    .with_counter("root_folders", folders.items.len() as u64)
                    .with_counter("metadata_profiles", metadata.items.len() as u64)
                    .with_counter("settings_cleared", cleared.len() as u64)
                    .with_item(ReportItem {
                        target,
                        outcome: ItemOutcome::Succeeded,
                        code: None,
                        detail: Some(detail),
                    }),
            )
        }
        Err(failure) => {
            ctx.db
                .store_source_lists_error(id, failure.code, now)
                .await?;
            tracing::warn!(service = id, code = failure.code, detail = %failure.detail, "reading source lists failed");
            Ok(Report::new(
                1,
                0,
                1,
                0,
                Expectation::Unmet(format!("reading the lists failed: {}", failure.code)),
            )?
            .with_item(ReportItem {
                target,
                outcome: ItemOutcome::Failed,
                code: Some(failure.code.to_owned()),
                detail: Some(failure.detail),
            }))
        }
    }
}
