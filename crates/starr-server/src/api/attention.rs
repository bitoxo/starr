//! The admin attention model (spec 0008 F9): one server-side list of what
//! needs an admin, used by the Inbox, its badge, `document.title` and the
//! admin bell, so their counts never disagree.
//!
//! `GET /api/v1/admin/attention` composes what the other admin endpoints
//! already know: requests waiting for approval, failed and stuck downloads,
//! categories that take no requests, account provisioning that needs a
//! hand, libraries and albums nobody maps, writes that are paused (with
//! what they hold back) and the first-run checklist. Private items stay
//! neutral rows while the session has not unlocked the private category
//! (spec 0007 Q9 B): they are listed and counted, but name nothing.

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::Serialize;
use starr_core::adapters::Capability;
use starr_core::categories::has_request_flow;
use starr_core::members::Category;
use starr_core::provisioning::WritesMode;
use starr_core::requests::AcquisitionState;
use starr_db::members::MemberFilter;
use starr_db::requests::RequestFilter;
use starr_worker::adapters;
use utoipa::ToSchema;

use super::account_writes::{
    AccountConflict, AccountDeviation, AccountWithoutPassword, FailedAccountWrite, HeldPlan,
    StaleLibrary, account_attention,
};
use super::common::CategoryDto;
use super::error::{ApiError, ErrorBody};
use super::libraries::{UnmappedLibrary, unmapped_libraries};
use super::privacy;
use super::requests::{
    AcquisitionDto, AdminRequest, UnlistedAlbumDto, admin_requests, source_target, unlisted_albums,
};
use super::services::{HeldAcquisitions, held_acquisitions};
use crate::AppState;
use crate::auth::{AdminSession, Session};

/// What an item is about (spec 0008 F9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// A request waits for approval (`request`).
    Approval,
    /// A download failed at the source (`acquisition`): submit it again.
    Failed,
    /// A download is stuck, e.g. its import is blocked (`acquisition`).
    Stuck,
    /// A switched-on category takes no requests: its source is missing or
    /// lacks a setting (`category`).
    CategoryClosed,
    /// A media-server account Starr manages or would create needs a
    /// decision (`conflict`), e.g. the name is taken.
    AccountConflict,
    /// An account write failed (`accountFailure`).
    AccountFailed,
    /// A change made in the media server that the next live run undoes
    /// (`deviation`).
    AccountDeviation,
    /// An account Starr created never got the member's password
    /// (`withoutPassword`).
    AccountWithoutPassword,
    /// A library no category maps (`library`).
    LibraryUnmapped,
    /// A mapped library the media server no longer lists (`staleLibrary`).
    LibraryStale,
    /// An album the music source has that no library lists (`album`).
    AlbumUnlisted,
    /// Writes to a service are paused (`writes`), with nothing held back
    /// (sources) or with account changes waiting (media servers).
    WritesPaused,
    /// Approved requests wait because the source's writes are paused
    /// (`writes`, with the held titles).
    Held,
    /// A live account plan waits for an admin's approval (`plan`).
    AccountPlanHeld,
    /// The first-run checklist (`setup`).
    Setup,
}

/// Where an item is listed in the Inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSection {
    Decide,
    Fix,
    Paused,
    Setup,
}

impl AttentionKind {
    #[must_use]
    pub const fn section(self) -> AttentionSection {
        match self {
            Self::Approval => AttentionSection::Decide,
            Self::Failed
            | Self::Stuck
            | Self::CategoryClosed
            | Self::AccountConflict
            | Self::AccountFailed
            | Self::AccountDeviation
            | Self::AccountWithoutPassword
            | Self::LibraryUnmapped
            | Self::LibraryStale
            | Self::AlbumUnlisted => AttentionSection::Fix,
            Self::WritesPaused | Self::Held | Self::AccountPlanHeld => AttentionSection::Paused,
            Self::Setup => AttentionSection::Setup,
        }
    }
}

/// Which writes are paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PausedWritesKind {
    /// Adding, monitoring and searching at a source (Radarr, Sonarr, …).
    Source,
    /// Account writes to a media server (Jellyfin, Emby, Navidrome).
    Accounts,
}

/// Paused writes of one service.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PausedWrites {
    service_id: i64,
    service_name: String,
    adapter_type: String,
    kind: PausedWritesKind,
    paused_at: Option<DateTime<Utc>>,
    /// The admin who paused them; absent when Starr did (`automatic`).
    paused_by: Option<String>,
    /// Starr paused them by itself: an active admin has no two-step
    /// verification (spec 0005 M10 1, ADM-06).
    automatic: bool,
    /// Account writes: active admins still without two-step verification.
    admins_without_totp: Vec<String>,
    /// The reason is gone: an admin may resume (account writes need every
    /// active admin to have two-step verification).
    can_resume: bool,
    /// Sources: what resuming sends out.
    held: Option<HeldAcquisitions>,
}

/// A category that takes no requests.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClosedCategory {
    category: CategoryDto,
    /// Its source when it has one (then a setting is missing).
    source_name: Option<String>,
}

/// One step of the first-run checklist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupStepKind {
    /// Connect a service.
    Services,
    /// Switch a category on.
    Categories,
    /// Two-step verification for the signed-in admin.
    TwoStep,
    /// Invite or add a member.
    Members,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetupStep {
    step: SetupStepKind,
    done: bool,
}

/// One thing that needs an admin. `kind` says which of the optional
/// fields carries its details.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttentionItem {
    /// Stable within its subject, e.g. `approval:12`: the Inbox's
    /// `?open=` value.
    id: String,
    kind: AttentionKind,
    section: AttentionSection,
    /// Since when it waits.
    since: Option<DateTime<Utc>>,
    /// About the private category while this session has not unlocked it:
    /// names nothing (spec 0007 Q9 B); show "Private category · request #12".
    redacted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    request: Option<AdminRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acquisition: Option<AcquisitionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<ClosedCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict: Option<AccountConflict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_failure: Option<FailedAccountWrite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deviation: Option<AccountDeviation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    without_password: Option<AccountWithoutPassword>,
    #[serde(skip_serializing_if = "Option::is_none")]
    library: Option<UnmappedLibrary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_library: Option<StaleLibrary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    album: Option<UnlistedAlbumDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    writes: Option<PausedWrites>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<HeldPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup: Option<Vec<SetupStep>>,
}

impl AttentionItem {
    fn new(id: String, kind: AttentionKind, since: Option<DateTime<Utc>>) -> Self {
        Self {
            id,
            kind,
            section: kind.section(),
            since,
            redacted: false,
            request: None,
            acquisition: None,
            category: None,
            conflict: None,
            account_failure: None,
            deviation: None,
            without_password: None,
            library: None,
            stale_library: None,
            album: None,
            writes: None,
            plan: None,
            setup: None,
        }
    }
}

/// Items per section.
#[derive(Debug, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SectionCounts {
    decide: u32,
    fix: u32,
    paused: u32,
    setup: u32,
}

/// Everything that needs an admin now.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminAttention {
    /// Items in all sections: the one count for the Inbox badge, heading,
    /// `document.title` and the admin bell. Neutral private rows count.
    total: u32,
    sections: SectionCounts,
    /// By section (decide, fix, paused, setup), oldest first within one.
    items: Vec<AttentionItem>,
}

/// Everything that needs an admin (spec 0008 F9), by section and oldest
/// first: approvals, failed and stuck downloads, closed categories,
/// account and library problems, paused writes with what they hold back,
/// and the first-run checklist. Private items are neutral rows unless this
/// session unlocked the private category (`redacted`).
#[utoipa::path(get, path = "/api/v1/admin/attention", tag = "admin",
    responses(
        (status = 200, body = AdminAttention),
        (status = 401, description = "`auth.unauthenticated`", body = ErrorBody),
        (status = 403, description = "`auth.forbidden` (not an admin)", body = ErrorBody),
        (status = 500, body = ErrorBody),
    ))]
pub async fn admin_attention(
    AdminSession(session): AdminSession,
    State(state): State<AppState>,
) -> Result<Json<AdminAttention>, ApiError> {
    let mut items = Vec::new();
    approvals(&state, &session, &mut items).await?;
    downloads(&state, &session, &mut items).await?;
    closed_categories(&state, &mut items).await?;
    accounts(&state, &mut items).await?;
    for library in unmapped_libraries(&state).await? {
        let mut item = AttentionItem::new(
            format!("library:{}:{}", library.service_id, library.library_id),
            AttentionKind::LibraryUnmapped,
            None,
        );
        item.library = Some(library);
        items.push(item);
    }
    for album in unlisted_albums(&state).await? {
        let mut item = AttentionItem::new(
            format!("album:{}", album.acquisition_id),
            AttentionKind::AlbumUnlisted,
            Some(album.since),
        );
        item.album = Some(album);
        items.push(item);
    }
    paused_writes(&state, &session, &mut items).await?;
    setup(&state, &session, &mut items).await?;

    // By section, oldest first; items without a time after the others.
    items.sort_by(|a, b| {
        a.section
            .cmp(&b.section)
            .then_with(|| match (a.since, b.since) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
    });
    let mut sections = SectionCounts::default();
    for item in &items {
        match item.section {
            AttentionSection::Decide => sections.decide += 1,
            AttentionSection::Fix => sections.fix += 1,
            AttentionSection::Paused => sections.paused += 1,
            AttentionSection::Setup => sections.setup += 1,
        }
    }
    Ok(Json(AdminAttention {
        total: u32::try_from(items.len()).unwrap_or(u32::MAX),
        sections,
        items,
    }))
}

/// Requests with items waiting for a decision, oldest first.
async fn approvals(
    state: &AppState,
    session: &Session,
    items: &mut Vec<AttentionItem>,
) -> Result<(), ApiError> {
    let waiting = state
        .db
        .list_requests(RequestFilter {
            needs_decision: true,
            oldest_first: true,
            ..RequestFilter::default()
        })
        .await?;
    for request in admin_requests(state, session, waiting).await? {
        let mut item = AttentionItem::new(
            format!("approval:{}", request.id),
            AttentionKind::Approval,
            Some(request.created_at),
        );
        item.redacted = request.redacted;
        item.request = Some(request);
        items.push(item);
    }
    Ok(())
}

/// Downloads someone still wants that failed at the source, or that the
/// source reports stuck (a blocked import, a stalled download).
async fn downloads(
    state: &AppState,
    session: &Session,
    items: &mut Vec<AttentionItem>,
) -> Result<(), ApiError> {
    let adult = privacy::adult_open(state, session).await?;
    for a in state.db.list_acquisitions(None, None).await? {
        if a.requests == 0 {
            continue;
        }
        let kind = match a.state {
            AcquisitionState::Failed => AttentionKind::Failed,
            AcquisitionState::Submitted | AcquisitionState::Downloading
                if a.source_message.is_some() =>
            {
                AttentionKind::Stuck
            }
            _ => continue,
        };
        let since = a.stage_since.unwrap_or(a.updated_at);
        let prefix = if kind == AttentionKind::Failed {
            "failed"
        } else {
            "stuck"
        };
        let dto = AcquisitionDto::new(a, adult);
        let mut item = AttentionItem::new(format!("{prefix}:{}", dto.id), kind, Some(since));
        item.redacted = dto.redacted;
        item.acquisition = Some(dto);
        items.push(item);
    }
    Ok(())
}

/// Switched-on categories with a request flow but no ready source.
async fn closed_categories(
    state: &AppState,
    items: &mut Vec<AttentionItem>,
) -> Result<(), ApiError> {
    for category in Category::ALL {
        if !has_request_flow(category) || !state.db.category_state(category).await?.enabled {
            continue;
        }
        if source_target(state, category).await?.is_some() {
            continue;
        }
        let source_name = state
            .db
            .list_roles(Some(category))
            .await?
            .into_iter()
            .find(|r| r.kind == starr_core::categories::RoleKind::Source)
            .map(|r| r.service_name);
        let mut item = AttentionItem::new(
            format!("category:{}", category.as_str()),
            AttentionKind::CategoryClosed,
            None,
        );
        item.category = Some(ClosedCategory {
            category: category.into(),
            source_name,
        });
        items.push(item);
    }
    Ok(())
}

/// Account provisioning that needs a hand (spec 0005 "needs you").
async fn accounts(state: &AppState, items: &mut Vec<AttentionItem>) -> Result<(), ApiError> {
    let a = account_attention(state).await?;
    for c in a.conflicts {
        let mut item = AttentionItem::new(
            format!("conflict:{}:{}", c.service_id, c.member_id),
            AttentionKind::AccountConflict,
            c.since,
        );
        item.conflict = Some(c);
        items.push(item);
    }
    for f in a.failures {
        let mut item = AttentionItem::new(
            format!("account-failed:{}:{}", f.service_id, f.member_id),
            AttentionKind::AccountFailed,
            Some(f.at),
        );
        item.account_failure = Some(f);
        items.push(item);
    }
    for d in a.deviations {
        let mut item = AttentionItem::new(
            format!("deviation:{}:{}", d.service_id, d.member_id),
            AttentionKind::AccountDeviation,
            None,
        );
        item.deviation = Some(d);
        items.push(item);
    }
    for w in a.without_password {
        let mut item = AttentionItem::new(
            format!("password:{}:{}", w.service_id, w.member_id),
            AttentionKind::AccountWithoutPassword,
            None,
        );
        item.without_password = Some(w);
        items.push(item);
    }
    for s in a.stale_libraries {
        let mut item = AttentionItem::new(
            format!("stale:{}:{}", s.service_id, s.library_id),
            AttentionKind::LibraryStale,
            None,
        );
        item.stale_library = Some(s);
        items.push(item);
    }
    for p in a.held {
        let mut item = AttentionItem::new(
            format!("plan:{}", p.service_id),
            AttentionKind::AccountPlanHeld,
            Some(p.computed_at),
        );
        item.plan = Some(p);
        items.push(item);
    }
    Ok(())
}

/// Services whose writes are paused: sources (with what they hold back)
/// and media servers' account writes (with whether the reason is gone).
async fn paused_writes(
    state: &AppState,
    session: &Session,
    items: &mut Vec<AttentionItem>,
) -> Result<(), ApiError> {
    let adult = privacy::adult_open(state, session).await?;
    let missing = super::service_accounts::admins_without_totp(state).await?;
    let account_instances: Vec<i64> = starr_worker::provisioning::account_instances(&state.db)
        .await?
        .into_iter()
        .map(|(id, ..)| id)
        .collect();
    for service in state.db.list_services().await? {
        let Some(paused_at) = service.writes_paused_at else {
            continue;
        };
        let automatic =
            service.writes_paused_by.as_deref() == Some(starr_worker::provisioning::AUTO_PAUSE_BY);
        let paused_by = service.writes_paused_by.clone().filter(|_| !automatic);
        let is_source = adapters::descriptor(&service.adapter_type)
            .is_some_and(|d| d.capabilities.contains(&Capability::Requests));
        let (kind, writes) = if is_source {
            let held = held_acquisitions(state, service.id, adult).await?;
            let kind = if held.total > 0 || held.stop_total > 0 {
                AttentionKind::Held
            } else {
                AttentionKind::WritesPaused
            };
            let redacted = held.redacted();
            (
                kind,
                (
                    redacted,
                    PausedWrites {
                        service_id: service.id,
                        service_name: service.name,
                        adapter_type: service.adapter_type,
                        kind: PausedWritesKind::Source,
                        paused_at: Some(paused_at),
                        paused_by,
                        automatic: false,
                        admins_without_totp: vec![],
                        can_resume: true,
                        held: Some(held),
                    },
                ),
            )
        } else if account_instances.contains(&service.id)
            && state
                .db
                .account_writes(service.id)
                .await?
                .is_some_and(|w| w.mode() == WritesMode::Paused)
        {
            (
                AttentionKind::WritesPaused,
                (
                    false,
                    PausedWrites {
                        service_id: service.id,
                        service_name: service.name,
                        adapter_type: service.adapter_type,
                        kind: PausedWritesKind::Accounts,
                        paused_at: Some(paused_at),
                        paused_by,
                        automatic,
                        admins_without_totp: missing.clone(),
                        can_resume: missing.is_empty(),
                        held: None,
                    },
                ),
            )
        } else {
            continue;
        };
        let (redacted, writes) = writes;
        let prefix = if kind == AttentionKind::Held {
            "held"
        } else {
            "writes"
        };
        let mut item = AttentionItem::new(
            format!("{prefix}:{}", writes.service_id),
            kind,
            Some(paused_at),
        );
        item.redacted = redacted;
        item.writes = Some(writes);
        items.push(item);
    }
    Ok(())
}

/// The first-run checklist, while a step is open: services, categories,
/// two-step verification for this admin, members.
async fn setup(
    state: &AppState,
    session: &Session,
    items: &mut Vec<AttentionItem>,
) -> Result<(), ApiError> {
    let services = !state.db.list_services().await?.is_empty();
    let mut categories = false;
    for category in Category::ALL {
        categories |= state.db.category_state(category).await?.enabled;
    }
    let two_step = !super::service_accounts::admins_without_totp(state)
        .await?
        .contains(&session.subject.username);
    let members = !state
        .db
        .list_members(MemberFilter::default())
        .await?
        .is_empty()
        || state
            .db
            .list_invitations()
            .await?
            .iter()
            .any(|i| i.used_at.is_none() && i.revoked_at.is_none());
    let steps = vec![
        SetupStep {
            step: SetupStepKind::Services,
            done: services,
        },
        SetupStep {
            step: SetupStepKind::Categories,
            done: categories,
        },
        SetupStep {
            step: SetupStepKind::TwoStep,
            done: two_step,
        },
        SetupStep {
            step: SetupStepKind::Members,
            done: members,
        },
    ];
    if steps.iter().all(|s| s.done) {
        return Ok(());
    }
    let mut item = AttentionItem::new("setup".to_owned(), AttentionKind::Setup, None);
    item.setup = Some(steps);
    items.push(item);
    Ok(())
}
