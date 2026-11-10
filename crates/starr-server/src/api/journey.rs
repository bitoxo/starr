//! Status journeys in the request DTOs (maintainer, 2026-11-02): the steps
//! behind every status chip, derived once per item by
//! [`starr_core::journey::journey`] from the request, its history and the
//! acquisition. Members get their own requests only and never anyone's
//! name (the rule leaves names out for [`Audience::Member`]).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use starr_core::journey::{
    self as rule, AcquisitionFacts, Audience, Journey, JourneyFacts, JourneyStep, Note,
    RequestFacts, Step, StepStatus,
};
use starr_core::requests::Decision;
use starr_db::requests::{
    AcquisitionRecord, EventTrigger, FOUND_BY_ARTIST_FOLLOW, FOUND_BY_FOLLOW,
    FOUND_BY_PRIVATE_FOLLOW, RequestEvent, RequestFilter, RequestItemRecord, RequestRecord,
};
use utoipa::ToSchema;

use super::catalog::MediaKindDto;
use super::error::ApiError;
use crate::AppState;

macro_rules! from_core {
    ($(#[$meta:meta])* $name:ident from $core:ty { $($variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl From<$core> for $name {
            fn from(v: $core) -> Self {
                match v {
                    $(<$core>::$variant => Self::$variant),+
                }
            }
        }
    };
}

from_core!(
    /// One point of a status journey. `getting_it` stands for searching,
    /// downloading and importing while none of them is current.
    JourneyStepKind from Step {
        Requested, AwaitingApproval, ApprovedAutomatically, Approved, Declined, Withdrawn,
        Held, Searching, Downloading, Importing, GettingIt, Arriving, Ready, Cancelled,
        Revoked, Removed,
    }
);

from_core!(
    /// How far a journey step is. `warning`: current but needs attention;
    /// `failed` and `ended`: the journey stops there.
    JourneyStepStatus from StepStatus {
        Done, Current, Upcoming, Warning, Failed, Ended,
    }
);

from_core!(
    /// A sentence shown with a step.
    JourneyNote from Note {
        ByFollow, CanWithdraw, AdminTold, StuckAtImport, PartlyReady, RequestAgain,
    }
);

/// One point of a status journey.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JourneyStepDto {
    step: JourneyStepKind,
    status: JourneyStepStatus,
    /// When it happened (done) or began (current), if known.
    at: Option<DateTime<Utc>>,
    /// Downloading: downloaded fraction 0–1.
    progress: Option<f64>,
    note: Option<JourneyNote>,
    /// Shown as is: the reject or revoke reason; for admins also the
    /// source's message or error.
    detail: Option<String>,
    /// Admins only: who approved, declined, cancelled or revoked.
    actor: Option<String>,
}

/// The steps behind a status chip, left to right.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JourneyDto {
    steps: Vec<JourneyStepDto>,
    /// Admins only: the source instance's name.
    source: Option<String>,
    /// The media server(s) the title shows up in.
    library: Option<String>,
}

/// The journey of one unit of a title: the movie, a season or an album.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnitJourney {
    /// The season; absent otherwise.
    season: Option<u32>,
    /// The album's release-group MBID; absent otherwise.
    album: Option<String>,
    journey: JourneyDto,
}

impl From<JourneyStep> for JourneyStepDto {
    fn from(s: JourneyStep) -> Self {
        Self {
            step: s.step.into(),
            status: s.status.into(),
            at: s.at,
            progress: s.progress,
            note: s.note.map(Into::into),
            detail: s.detail,
            actor: s.actor,
        }
    }
}

impl From<Journey> for JourneyDto {
    fn from(j: Journey) -> Self {
        Self {
            steps: j.steps.into_iter().map(Into::into).collect(),
            source: j.source,
            library: j.library,
        }
    }
}

/// Whether the request was made by the member's follow (its first event).
fn by_follow(events: &[RequestEvent]) -> bool {
    events
        .first()
        .and_then(|e| e.detail.as_deref())
        .is_some_and(|d| {
            d.split("; ").any(|part| {
                [
                    FOUND_BY_FOLLOW,
                    FOUND_BY_ARTIST_FOLLOW,
                    FOUND_BY_PRIVATE_FOLLOW,
                ]
                .contains(&part)
            })
        })
}

/// The admin who decided the item: the admin event at its decision time.
fn decided_by<'a>(item: &RequestItemRecord, events: &'a [RequestEvent]) -> Option<&'a str> {
    let at = item.decided_at?;
    events
        .iter()
        .find(|e| e.trigger == EventTrigger::Admin && e.at == at)
        .and_then(|e| e.actor.as_deref())
}

/// Who ended the item (cancelled, revoked, removed): the event at its end.
fn ended_by<'a>(item: &RequestItemRecord, events: &'a [RequestEvent]) -> Option<&'a str> {
    let at = item.ended_at?;
    events
        .iter()
        .rev()
        .find(|e| e.at == at)
        .and_then(|e| e.actor.as_deref())
}

/// The journey of `item` of `request` for `audience`.
pub(crate) fn item_journey(
    audience: Audience,
    request: &RequestRecord,
    item: &RequestItemRecord,
    events: &[RequestEvent],
) -> JourneyDto {
    let acquisition = item.acquisition.as_ref();
    rule::journey(&JourneyFacts {
        audience,
        request: Some(RequestFacts {
            requested_at: request.created_at,
            by_follow: by_follow(events),
            decision: item.decision,
            decided_at: item.decided_at,
            decided_by: decided_by(item, events),
            reject_reason: request
                .reject_reason
                .as_deref()
                .filter(|_| item.decision == Decision::Rejected),
            ended_at: item.ended_at,
            ended_by: ended_by(item, events),
            revoke_reason: request
                .revoke_reason
                .as_deref()
                .filter(|_| item.decision == Decision::Revoked),
        }),
        acquisition: acquisition.map(|a| AcquisitionFacts {
            state: a.state,
            stage: a.stage,
            progress: a.progress,
            held: a.held,
            source_message: a.source_message.as_deref(),
            last_error: a.last_error.as_deref(),
            created_at: a.created_at,
            submitted_at: a.submitted_at,
            available_at: a.available_at,
            stage_since: a.stage_since,
        }),
        source: acquisition.and_then(|a| a.service_name.as_deref()),
        library: acquisition.and_then(|a| a.library_name.as_deref()),
    })
    .into()
}

/// The journey of an acquisition on its own (admins).
pub(crate) fn acquisition_journey(a: &AcquisitionRecord) -> JourneyDto {
    rule::journey(&JourneyFacts {
        audience: Audience::Admin,
        request: None,
        acquisition: Some(AcquisitionFacts {
            state: a.state,
            stage: a.stage,
            progress: a.progress,
            held: a.held,
            source_message: a.source_message.as_deref(),
            last_error: a.last_error.as_deref(),
            created_at: a.created_at,
            submitted_at: a.submitted_at,
            available_at: a.available_at,
            stage_since: a.stage_since,
        }),
        source: a.service_name.as_deref(),
        library: a.library_name.as_deref(),
    })
    .into()
}

/// A member's own requests with their history, for the journeys on title
/// cards and the detail view.
pub(crate) struct MemberRequests {
    requests: Vec<RequestRecord>,
    events: BTreeMap<i64, Vec<RequestEvent>>,
}

impl MemberRequests {
    /// The member's requests with open items: all of them (title cards), or
    /// those for one title (`catalog_item_id`, the detail view).
    pub(crate) async fn load(
        state: &AppState,
        member_id: i64,
        catalog_item_id: Option<i64>,
    ) -> Result<Self, ApiError> {
        let requests = state
            .db
            .list_requests(RequestFilter {
                member_id: Some(member_id),
                open: true,
                catalog_item_id,
                // Title cards and detail views outside the private area
                // never show a private request (spec 0007 Q11).
                without_category: Some(starr_core::members::Category::Adult),
                ..RequestFilter::default()
            })
            .await?;
        let ids: Vec<i64> = requests.iter().map(|r| r.id).collect();
        let events = state.db.requests_events(&ids).await?;
        Ok(Self { requests, events })
    }

    /// The journey of every unit of the title the member has an open
    /// (pending or approved) item for, by season; newest request first.
    pub(crate) fn title(&self, kind: MediaKindDto, title_id: &str) -> Vec<UnitJourney> {
        self.units(|r, _| {
            super::requests::title_id_of(r).as_deref() == Some(title_id)
                && super::requests::media(r.kind) == Some(kind)
        })
    }

    /// The journey of album `release_group` (an item of an artist's
    /// request), if the member has it open.
    pub(crate) fn album(&self, release_group: uuid::Uuid) -> Vec<UnitJourney> {
        self.units(|_, item| {
            item.album
                .as_ref()
                .is_some_and(|a| a.release_group == release_group)
        })
    }

    /// The journeys of the open items `wanted` picks, one per unit (season
    /// or album); newest request first.
    fn units(
        &self,
        wanted: impl Fn(&RequestRecord, &RequestItemRecord) -> bool,
    ) -> Vec<UnitJourney> {
        let mut units: BTreeMap<(Option<u32>, Option<uuid::Uuid>), UnitJourney> = BTreeMap::new();
        for request in &self.requests {
            let events = self.events.get(&request.id).map_or(&[][..], Vec::as_slice);
            for item in request.items.iter().filter(|i| {
                matches!(i.decision, Decision::PendingApproval | Decision::Approved)
                    && wanted(request, i)
            }) {
                let album = item.album.as_ref().map(|a| a.release_group);
                units
                    .entry((item.season_number, album))
                    .or_insert_with(|| UnitJourney {
                        season: item.season_number,
                        album: album.map(|a| a.hyphenated().to_string()),
                        journey: item_journey(Audience::Member, request, item, events),
                    });
            }
        }
        units.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use starr_core::requests::RequestState;

    use super::*;

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 15, minute, 0)
            .single()
            .unwrap_or_default()
    }

    fn event(
        minute: u32,
        trigger: EventTrigger,
        actor: &str,
        detail: Option<&str>,
    ) -> RequestEvent {
        RequestEvent {
            at: at(minute),
            from_state: None,
            to_state: RequestState::PendingApproval,
            trigger,
            actor: Some(actor.to_owned()),
            detail: detail.map(str::to_owned),
        }
    }

    fn item(decided_at: Option<DateTime<Utc>>) -> RequestItemRecord {
        RequestItemRecord {
            id: 1,
            catalog_item_id: 1,
            season_number: Some(9),
            album: None,
            decision: Decision::Approved,
            state: RequestState::Approved,
            decided_at,
            ended_at: None,
            acquisition: None,
        }
    }

    #[test]
    fn follow_and_deciding_admin_come_from_the_history() {
        let found = format!("{FOUND_BY_FOLLOW}; 1 attached to running acquisitions");
        let events = [
            event(10, EventTrigger::System, "follows.check", Some(&found)),
            event(20, EventTrigger::Admin, "root", Some("1 item(s)")),
        ];
        assert!(by_follow(&events));
        assert!(!by_follow(&events[1..]));
        assert_eq!(decided_by(&item(Some(at(20))), &events), Some("root"));
        // Approved together with the request: no admin decided.
        assert_eq!(decided_by(&item(Some(at(10))), &events), None);
        assert_eq!(decided_by(&item(None), &events), None);
    }
}
