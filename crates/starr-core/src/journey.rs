//! The status journey of one request item (maintainer, 2026-11-02): which
//! steps a member or an admin sees behind a status chip, which of them are
//! done, which one is current and what comes next. Pure rule; the server
//! computes it once per item from the request's decision and history and
//! the acquisition's live state, the web only renders it.
//!
//! At most about five points: searching, downloading and importing collapse
//! into one "getting it" step unless one of them is current. Branches are
//! notes on a step, not extra paths: a declined or withdrawn item ends after
//! "requested", a failed one ends at the affected step. Items ended after
//! approval (spec 0004) end after their last step: cancelled (muted),
//! revoked (red, with the reason) or removed from the library (muted, "you
//! can request it again").
//!
//! Members never see other members, admins' names, source names or raw
//! source errors; [`Audience::Member`] leaves them out here, once.

use chrono::{DateTime, Utc};

use crate::requests::{AcquisitionState, Decision, DownloadStage};

/// Who looks at the journey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    Member,
    /// Technical labels, actor names and the source's messages.
    Admin,
}

/// One point of the journey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Requested,
    /// Waits for an admin (request mode `approval`).
    AwaitingApproval,
    /// Approved on request: request mode `auto`, or attached to a running
    /// acquisition.
    ApprovedAutomatically,
    /// Approved by an admin.
    Approved,
    Declined,
    Withdrawn,
    /// Admins only: pending at a source whose writes are paused.
    Held,
    /// Handed to the source, nothing downloading yet.
    Searching,
    Downloading,
    Importing,
    /// Searching, downloading and importing in one point while none of
    /// them is current ("Getting it", done: "Downloaded").
    GettingIt,
    /// The source has the files, the media server does not list them yet.
    Arriving,
    Ready,
    /// The member cancelled after approval; for an acquisition on its own:
    /// nobody wants it any more (spec 0004).
    Cancelled,
    /// An admin revoked it, with a reason (spec 0004 R3).
    Revoked,
    /// It left the library (spec 0004 R4).
    Removed,
}

/// How far a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Done,
    Current,
    Upcoming,
    /// Current, but needs attention (held, stuck import).
    Warning,
    /// The journey ends here in failure (declined, failed).
    Failed,
    /// The journey ends here quietly (withdrawn).
    Ended,
}

/// A sentence the web shows with a step (by key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// The request was made by the member's follow of the series.
    ByFollow,
    /// Members can withdraw until it is approved (D11).
    CanWithdraw,
    /// Failed or stuck: members are told the admin knows.
    AdminTold,
    /// The source cannot import the finished download.
    StuckAtImport,
    /// Some episodes are watchable, more follow.
    PartlyReady,
    /// No longer in the library: it can be requested again.
    RequestAgain,
}

/// One point with its status.
#[derive(Debug, Clone, PartialEq)]
pub struct JourneyStep {
    pub step: Step,
    pub status: StepStatus,
    /// When it happened (done steps) or began (current steps), if known.
    pub at: Option<DateTime<Utc>>,
    /// Downloading: downloaded fraction 0–1.
    pub progress: Option<f64>,
    pub note: Option<Note>,
    /// Text shown as is: the reject reason (everyone), the source's message
    /// or error (admins only).
    pub detail: Option<String>,
    /// Admins only: who decided.
    pub actor: Option<String>,
}

impl JourneyStep {
    const fn new(step: Step, status: StepStatus) -> Self {
        Self {
            step,
            status,
            at: None,
            progress: None,
            note: None,
            detail: None,
            actor: None,
        }
    }

    const fn at(mut self, at: Option<DateTime<Utc>>) -> Self {
        self.at = at;
        self
    }

    const fn note(mut self, note: Note) -> Self {
        self.note = Some(note);
        self
    }

    fn detail(mut self, detail: Option<&str>) -> Self {
        self.detail = detail.map(str::to_owned);
        self
    }
}

/// The journey of one item, with the names its labels need.
#[derive(Debug, Clone, PartialEq)]
pub struct Journey {
    pub steps: Vec<JourneyStep>,
    /// Admins only: the source instance ("Submitted to Radarr").
    pub source: Option<String>,
    /// The media server(s) it shows up in ("arriving in Jellyfin").
    pub library: Option<String>,
}

/// What the request says about the item.
#[derive(Debug, Clone, Copy)]
pub struct RequestFacts<'a> {
    pub requested_at: DateTime<Utc>,
    /// Created by the member's follow of the series (`follows.check`).
    pub by_follow: bool,
    pub decision: Decision,
    pub decided_at: Option<DateTime<Utc>>,
    /// The admin who decided, from the request's history; `None` when no
    /// admin did (request mode `auto`, attaching, withdrawing).
    pub decided_by: Option<&'a str>,
    pub reject_reason: Option<&'a str>,
    /// When an approved item was cancelled, revoked or removed (spec 0004).
    pub ended_at: Option<DateTime<Utc>>,
    /// Who ended it, from the request's history (shown to admins only).
    pub ended_by: Option<&'a str>,
    /// Why an admin revoked it (shown to everyone, like a reject reason).
    pub revoke_reason: Option<&'a str>,
}

/// The live state of the item's acquisition.
#[derive(Debug, Clone, Copy)]
pub struct AcquisitionFacts<'a> {
    pub state: AcquisitionState,
    pub stage: Option<DownloadStage>,
    pub progress: Option<f64>,
    /// Pending at a source whose writes are paused.
    pub held: bool,
    /// The source's message about a queue warning.
    pub source_message: Option<&'a str>,
    /// Why the submission failed.
    pub last_error: Option<&'a str>,
    pub created_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
    /// When the current download stage began.
    pub stage_since: Option<DateTime<Utc>>,
}

/// Everything the journey is derived from.
#[derive(Debug, Clone, Copy)]
pub struct JourneyFacts<'a> {
    pub audience: Audience,
    /// `None` for an acquisition on its own (the admin's acquisitions).
    pub request: Option<RequestFacts<'a>>,
    pub acquisition: Option<AcquisitionFacts<'a>>,
    /// The source instance's name.
    pub source: Option<&'a str>,
    /// The category's media servers by name.
    pub library: Option<&'a str>,
}

/// Where an approved item stands.
enum Phase {
    /// Approved, not handed to the source yet.
    Waiting {
        held: bool,
    },
    Searching,
    Downloading,
    Importing,
    StuckImport,
    Arriving,
    PartlyReady,
    Ready,
    Failed,
    /// Nobody wants it any more.
    Cancelled,
}

fn phase(acquisition: Option<&AcquisitionFacts<'_>>) -> Phase {
    let Some(a) = acquisition else {
        return Phase::Waiting { held: false };
    };
    match (a.state, a.stage) {
        (AcquisitionState::Pending, _) => Phase::Waiting { held: a.held },
        (AcquisitionState::Submitted, _) => Phase::Searching,
        (AcquisitionState::Downloading, Some(DownloadStage::Importing)) => Phase::Importing,
        (AcquisitionState::Downloading, Some(DownloadStage::Arriving)) => Phase::Arriving,
        // A blocked import shows as a finished download with a warning.
        (AcquisitionState::Downloading, None)
            if a.source_message.is_some() && a.progress.is_some_and(|p| p >= 1.0) =>
        {
            Phase::StuckImport
        }
        (AcquisitionState::Downloading, None) => Phase::Downloading,
        (AcquisitionState::PartiallyAvailable, _) => Phase::PartlyReady,
        (AcquisitionState::Available, _) => Phase::Ready,
        (AcquisitionState::Failed, _) => Phase::Failed,
        (AcquisitionState::Cancelled, _) => Phase::Cancelled,
    }
}

/// The journey of one item for `facts.audience`.
#[must_use]
pub fn journey(facts: &JourneyFacts<'_>) -> Journey {
    let admin = facts.audience == Audience::Admin;
    let phase = phase(facts.acquisition.as_ref());
    let mut steps = Vec::with_capacity(6);
    // The decision step is current while the source has not got it yet.
    let decided = match phase {
        Phase::Waiting { held } if !(held && admin) => StepStatus::Current,
        _ => StepStatus::Done,
    };
    let approved = if let Some(r) = facts.request {
        request_steps(&r, facts.acquisition.as_ref(), admin, decided, &mut steps)
    } else {
        let at = facts.acquisition.map(|a| a.created_at);
        steps.push(JourneyStep::new(Step::Approved, decided).at(at));
        true
    };
    if approved {
        acquisition_steps(facts, &phase, &mut steps);
    }
    let library = facts.library.map(str::to_owned);
    Journey {
        steps,
        source: facts.source.filter(|_| admin).map(str::to_owned),
        library,
    }
}

/// The approval step: automatic (mode `auto`, attached) or by an admin.
fn approved_step(r: &RequestFacts<'_>, admin: bool, status: StepStatus) -> JourneyStep {
    // Approved together with the request and by no admin: mode `auto`, or
    // attached to a running acquisition. `decided_at` is always written
    // with the decision; without it (no such rows today) the approval
    // counts as automatic.
    let automatic = r.decided_by.is_none() && r.decided_at.is_none_or(|at| at == r.requested_at);
    let mut approved = if automatic {
        JourneyStep::new(Step::ApprovedAutomatically, status)
    } else {
        let mut step = JourneyStep::new(Step::Approved, status);
        step.actor = r.decided_by.filter(|_| admin).map(str::to_owned);
        step
    };
    approved.at = r.decided_at;
    approved
}

/// "Requested" and the decision; `false` when the journey ends there
/// (waiting, declined, withdrawn, or ended after approval).
fn request_steps(
    r: &RequestFacts<'_>,
    acquisition: Option<&AcquisitionFacts<'_>>,
    admin: bool,
    decided: StepStatus,
    steps: &mut Vec<JourneyStep>,
) -> bool {
    let mut requested =
        JourneyStep::new(Step::Requested, StepStatus::Done).at(Some(r.requested_at));
    if r.by_follow {
        requested = requested.note(Note::ByFollow);
    }
    steps.push(requested);
    let actor = r.decided_by.filter(|_| admin).map(str::to_owned);
    match r.decision {
        Decision::PendingApproval => {
            let mut waiting = JourneyStep::new(Step::AwaitingApproval, StepStatus::Current);
            if !admin {
                waiting = waiting.note(Note::CanWithdraw);
            }
            steps.push(waiting);
            upcoming(steps, &[Step::GettingIt, Step::Arriving, Step::Ready]);
            false
        }
        Decision::Rejected => {
            let mut declined = JourneyStep::new(Step::Declined, StepStatus::Failed)
                .at(r.decided_at)
                .detail(r.reject_reason);
            declined.actor = actor;
            steps.push(declined);
            false
        }
        // Withdrawn before approval: no acquisition was ever linked.
        Decision::Cancelled if acquisition.is_none() => {
            steps.push(
                JourneyStep::new(Step::Withdrawn, StepStatus::Ended)
                    .at(r.ended_at.or(r.decided_at)),
            );
            false
        }
        Decision::Cancelled | Decision::Revoked | Decision::Removed => {
            ended_steps(r, acquisition, admin, steps);
            false
        }
        Decision::Approved => {
            steps.push(approved_step(r, admin, decided));
            true
        }
    }
}

/// The end of an item cancelled, revoked or removed (spec 0004): the
/// approval when there was one, for a removed item the time it was ready,
/// and the ending itself.
fn ended_steps(
    r: &RequestFacts<'_>,
    acquisition: Option<&AcquisitionFacts<'_>>,
    admin: bool,
    steps: &mut Vec<JourneyStep>,
) {
    if acquisition.is_some() {
        steps.push(approved_step(r, admin, StepStatus::Done));
    }
    let mut end = match r.decision {
        Decision::Revoked => {
            JourneyStep::new(Step::Revoked, StepStatus::Failed).detail(r.revoke_reason)
        }
        Decision::Removed => {
            steps.push(
                JourneyStep::new(Step::Ready, StepStatus::Done)
                    .at(acquisition.and_then(|a| a.available_at)),
            );
            JourneyStep::new(Step::Removed, StepStatus::Ended).note(Note::RequestAgain)
        }
        _ => JourneyStep::new(Step::Cancelled, StepStatus::Ended),
    }
    .at(r.ended_at);
    end.actor = r.ended_by.filter(|_| admin).map(str::to_owned);
    steps.push(end);
}

/// The steps after approval, from the acquisition's live state.
fn acquisition_steps(facts: &JourneyFacts<'_>, phase: &Phase, steps: &mut Vec<JourneyStep>) {
    let admin = facts.audience == Audience::Admin;
    let acquisition = facts.acquisition.as_ref();
    // Shared acquisitions may predate the member's request or its approval
    // (attached to one running or available already); times before those
    // would read as if the steps went back in time.
    let since = facts.request.map(|r| {
        r.decided_at
            .map_or(r.requested_at, |d| d.max(r.requested_at))
    });
    let after = |at: Option<DateTime<Utc>>| at.filter(|t| since.is_none_or(|s| *t >= s));
    let message = acquisition.and_then(|a| a.source_message).filter(|_| admin);
    let stage_since = after(acquisition.and_then(|a| a.stage_since));
    match *phase {
        Phase::Waiting { held } => {
            if held && admin {
                steps.push(JourneyStep::new(Step::Held, StepStatus::Warning));
            }
            upcoming(steps, &[Step::GettingIt, Step::Arriving, Step::Ready]);
        }
        Phase::Searching => current(
            steps,
            JourneyStep::new(Step::Searching, StepStatus::Current)
                .at(after(acquisition.and_then(|a| a.submitted_at))),
            &[Step::Arriving, Step::Ready],
        ),
        Phase::Downloading => {
            let status = if message.is_some() {
                StepStatus::Warning
            } else {
                StepStatus::Current
            };
            let mut step = JourneyStep::new(Step::Downloading, status).detail(message);
            step.progress = acquisition.and_then(|a| a.progress);
            current(steps, step, &[Step::Arriving, Step::Ready]);
        }
        Phase::Importing => current(
            steps,
            JourneyStep::new(Step::Importing, StepStatus::Current).at(stage_since),
            &[Step::Arriving, Step::Ready],
        ),
        Phase::StuckImport => {
            let step = JourneyStep::new(Step::Importing, StepStatus::Warning);
            let step = if admin {
                step.detail(message)
            } else {
                step.note(Note::StuckAtImport)
            };
            current(steps, step, &[Step::Arriving, Step::Ready]);
        }
        Phase::Arriving => {
            steps.push(JourneyStep::new(Step::GettingIt, StepStatus::Done).at(stage_since));
            current(
                steps,
                JourneyStep::new(Step::Arriving, StepStatus::Current).at(stage_since),
                &[Step::Ready],
            );
        }
        Phase::PartlyReady => {
            steps.push(JourneyStep::new(Step::GettingIt, StepStatus::Done));
            steps.push(JourneyStep::new(Step::Ready, StepStatus::Current).note(Note::PartlyReady));
        }
        Phase::Ready => {
            steps.push(JourneyStep::new(Step::GettingIt, StepStatus::Done));
            steps.push(
                JourneyStep::new(Step::Ready, StepStatus::Done)
                    .at(after(acquisition.and_then(|a| a.available_at))),
            );
        }
        Phase::Cancelled => {
            steps.push(JourneyStep::new(Step::Cancelled, StepStatus::Ended));
        }
        // Only a submission fails today (`record_submit_failure`), so the
        // failure belongs to "searching" (handing it to the source).
        Phase::Failed => {
            let step = JourneyStep::new(Step::Searching, StepStatus::Failed);
            steps.push(if admin {
                step.detail(acquisition.and_then(|a| a.last_error))
            } else {
                step.note(Note::AdminTold)
            });
        }
    }
}

/// The current step and what follows it.
fn current(steps: &mut Vec<JourneyStep>, step: JourneyStep, next: &[Step]) {
    steps.push(step);
    upcoming(steps, next);
}

fn upcoming(steps: &mut Vec<JourneyStep>, next: &[Step]) {
    steps.extend(
        next.iter()
            .map(|step| JourneyStep::new(*step, StepStatus::Upcoming)),
    );
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    use AcquisitionState as A;
    use Step::{
        Approved, ApprovedAutomatically, Arriving, AwaitingApproval, Declined, Downloading,
        GettingIt, Held, Importing, Ready, Requested, Searching, Withdrawn,
    };
    use StepStatus::{Current, Done, Ended, Failed, Upcoming, Warning};

    fn t(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 15, minute, 0)
            .single()
            .unwrap_or_default()
    }

    fn request(decision: Decision, decided_at: Option<DateTime<Utc>>) -> RequestFacts<'static> {
        RequestFacts {
            requested_at: t(10),
            by_follow: false,
            decision,
            decided_at,
            decided_by: Some("root"),
            reject_reason: None,
            ended_at: None,
            ended_by: None,
            revoke_reason: None,
        }
    }

    fn ended(decision: Decision, by: &'static str) -> RequestFacts<'static> {
        let mut r = request(decision, Some(t(30)));
        r.ended_at = Some(t(50));
        r.ended_by = Some(by);
        r
    }

    #[test]
    fn items_ended_after_approval_end_after_their_last_step() {
        use Step::{Cancelled, Removed, Revoked};
        let cancelled = journey(&facts(
            Audience::Member,
            Some(ended(Decision::Cancelled, "alice")),
            Some(acquisition(A::Submitted)),
        ));
        assert_eq!(
            shape(&cancelled),
            vec![(Requested, Done), (Approved, Done), (Cancelled, Ended)]
        );
        assert_eq!(cancelled.steps[2].at, Some(t(50)));
        // Members never see who ended it.
        assert!(cancelled.steps.iter().all(|s| s.actor.is_none()));

        let mut revoked_facts = ended(Decision::Revoked, "root");
        revoked_facts.revoke_reason = Some("not for this server");
        for audience in [Audience::Member, Audience::Admin] {
            let revoked = journey(&facts(
                audience,
                Some(revoked_facts),
                Some(acquisition(A::Downloading)),
            ));
            assert_eq!(
                shape(&revoked),
                vec![(Requested, Done), (Approved, Done), (Revoked, Failed)]
            );
            // The reason is for everyone, like a reject reason.
            assert_eq!(
                revoked.steps[2].detail.as_deref(),
                Some("not for this server")
            );
            let actor = revoked.steps[2].actor.as_deref();
            assert_eq!(actor, (audience == Audience::Admin).then_some("root"));
        }

        // Revoked while it waited for approval: no approval step.
        let mut pending = ended(Decision::Revoked, "root");
        pending.decided_at = None;
        let early = journey(&facts(Audience::Member, Some(pending), None));
        assert_eq!(shape(&early), vec![(Requested, Done), (Revoked, Failed)]);

        let mut gone = acquisition(A::Cancelled);
        gone.available_at = Some(t(40));
        let removed = journey(&facts(
            Audience::Member,
            Some(ended(Decision::Removed, "acquisitions.sync")),
            Some(gone),
        ));
        assert_eq!(
            shape(&removed),
            vec![
                (Requested, Done),
                (Approved, Done),
                (Ready, Done),
                (Removed, Ended)
            ]
        );
        assert_eq!(removed.steps[2].at, Some(t(40)));
        assert_eq!(removed.steps[3].note, Some(Note::RequestAgain));

        // An acquisition nobody wants any more ends cancelled.
        let unwanted = journey(&facts(
            Audience::Admin,
            None,
            Some(acquisition(A::Cancelled)),
        ));
        assert_eq!(shape(&unwanted), vec![(Approved, Done), (Cancelled, Ended)]);
    }

    fn acquisition(state: AcquisitionState) -> AcquisitionFacts<'static> {
        AcquisitionFacts {
            state,
            stage: None,
            progress: None,
            held: false,
            source_message: None,
            last_error: None,
            created_at: t(20),
            submitted_at: None,
            available_at: None,
            stage_since: None,
        }
    }

    fn facts<'a>(
        audience: Audience,
        request: Option<RequestFacts<'a>>,
        acquisition: Option<AcquisitionFacts<'a>>,
    ) -> JourneyFacts<'a> {
        JourneyFacts {
            audience,
            request,
            acquisition,
            source: Some("Radarr"),
            library: Some("Jellyfin"),
        }
    }

    fn shape(j: &Journey) -> Vec<(Step, StepStatus)> {
        j.steps.iter().map(|s| (s.step, s.status)).collect()
    }

    fn auto() -> RequestFacts<'static> {
        let mut r = request(Decision::Approved, Some(t(10)));
        r.decided_by = None;
        r
    }

    fn approved_by_admin() -> RequestFacts<'static> {
        request(Decision::Approved, Some(t(30)))
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one table of cases
    fn steps_follow_the_request_and_its_acquisition() {
        type Case = (
            &'static str,
            Option<RequestFacts<'static>>,
            Option<AcquisitionFacts<'static>>,
            Vec<(Step, StepStatus)>,
        );
        let with = |state, f: fn(&mut AcquisitionFacts<'static>)| {
            let mut a = acquisition(state);
            f(&mut a);
            Some(a)
        };
        let cases: Vec<Case> = vec![
            (
                "waiting for approval",
                Some(request(Decision::PendingApproval, None)),
                None,
                vec![
                    (Requested, Done),
                    (AwaitingApproval, Current),
                    (GettingIt, Upcoming),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "declined ends after requested",
                Some(request(Decision::Rejected, Some(t(30)))),
                None,
                vec![(Requested, Done), (Declined, Failed)],
            ),
            (
                "withdrawn ends quietly",
                Some(request(Decision::Cancelled, Some(t(30)))),
                None,
                vec![(Requested, Done), (Withdrawn, Ended)],
            ),
            (
                "approved, not submitted yet",
                Some(approved_by_admin()),
                with(A::Pending, |_| {}),
                vec![
                    (Requested, Done),
                    (Approved, Current),
                    (GettingIt, Upcoming),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "auto mode",
                Some(auto()),
                with(A::Pending, |_| {}),
                vec![
                    (Requested, Done),
                    (ApprovedAutomatically, Current),
                    (GettingIt, Upcoming),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "members never see held",
                Some(approved_by_admin()),
                with(A::Pending, |a| a.held = true),
                vec![
                    (Requested, Done),
                    (Approved, Current),
                    (GettingIt, Upcoming),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "searching",
                Some(approved_by_admin()),
                with(A::Submitted, |_| {}),
                vec![
                    (Requested, Done),
                    (Approved, Done),
                    (Searching, Current),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "downloading",
                Some(approved_by_admin()),
                with(A::Downloading, |a| a.progress = Some(0.4)),
                vec![
                    (Requested, Done),
                    (Approved, Done),
                    (Downloading, Current),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "importing",
                Some(approved_by_admin()),
                with(A::Downloading, |a| a.stage = Some(DownloadStage::Importing)),
                vec![
                    (Requested, Done),
                    (Approved, Done),
                    (Importing, Current),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "import blocked by the source",
                Some(approved_by_admin()),
                with(A::Downloading, |a| {
                    a.progress = Some(1.0);
                    a.source_message = Some("No files found are eligible for import");
                }),
                vec![
                    (Requested, Done),
                    (Approved, Done),
                    (Importing, Warning),
                    (Arriving, Upcoming),
                    (Ready, Upcoming),
                ],
            ),
            (
                "arriving",
                Some(approved_by_admin()),
                with(A::Downloading, |a| a.stage = Some(DownloadStage::Arriving)),
                vec![
                    (Requested, Done),
                    (Approved, Done),
                    (GettingIt, Done),
                    (Arriving, Current),
                    (Ready, Upcoming),
                ],
            ),
            (
                "partly ready",
                Some(approved_by_admin()),
                with(A::PartiallyAvailable, |_| {}),
                vec![
                    (Requested, Done),
                    (Approved, Done),
                    (GettingIt, Done),
                    (Ready, Current),
                ],
            ),
            (
                "ready",
                Some(approved_by_admin()),
                with(A::Available, |_| {}),
                vec![
                    (Requested, Done),
                    (Approved, Done),
                    (GettingIt, Done),
                    (Ready, Done),
                ],
            ),
            (
                "failed at the source",
                Some(approved_by_admin()),
                with(A::Failed, |a| a.last_error = Some("Radarr refused")),
                vec![(Requested, Done), (Approved, Done), (Searching, Failed)],
            ),
        ];
        for (name, request, acquisition, expected) in cases {
            let j = journey(&facts(Audience::Member, request, acquisition));
            assert_eq!(shape(&j), expected, "{name}");
            assert!(j.steps.len() <= 5, "{name}: at most five points");
            let current = j
                .steps
                .iter()
                .filter(|s| matches!(s.status, Current | Warning))
                .count();
            assert!(current <= 1, "{name}: one current step");
        }
    }

    #[test]
    fn notes_and_times_for_members() {
        let mut r = request(Decision::PendingApproval, None);
        r.by_follow = true;
        let j = journey(&facts(Audience::Member, Some(r), None));
        assert_eq!(j.steps[0].note, Some(Note::ByFollow));
        assert_eq!(j.steps[0].at, Some(t(10)));
        assert_eq!(j.steps[1].note, Some(Note::CanWithdraw));

        let mut declined = request(Decision::Rejected, Some(t(30)));
        declined.reject_reason = Some("Not in 4K");
        let j = journey(&facts(Audience::Member, Some(declined), None));
        assert_eq!(j.steps[1].detail.as_deref(), Some("Not in 4K"));
        assert_eq!(j.steps[1].at, Some(t(30)));

        let mut a = acquisition(A::Downloading);
        a.progress = Some(0.42);
        let j = journey(&facts(Audience::Member, Some(approved_by_admin()), Some(a)));
        assert_eq!(j.steps[2].progress, Some(0.42));

        // Ready: the time it became available.
        let mut a = acquisition(A::Available);
        a.available_at = Some(t(50));
        let j = journey(&facts(Audience::Member, Some(approved_by_admin()), Some(a)));
        assert_eq!(j.steps[3].at, Some(t(50)));
        assert_eq!(j.library.as_deref(), Some("Jellyfin"));

        let j = journey(&facts(
            Audience::Member,
            Some(approved_by_admin()),
            Some(acquisition(A::PartiallyAvailable)),
        ));
        assert_eq!(j.steps[3].note, Some(Note::PartlyReady));
    }

    #[test]
    fn shared_acquisition_times_never_predate_the_request() {
        // Another member's acquisition was submitted before this request.
        let mut a = acquisition(A::Submitted);
        a.submitted_at = Some(t(5));
        let j = journey(&facts(Audience::Member, Some(auto()), Some(a)));
        assert_eq!(j.steps[2].at, None);
        a.submitted_at = Some(t(15));
        let j = journey(&facts(Audience::Member, Some(auto()), Some(a)));
        assert_eq!(j.steps[2].at, Some(t(15)));
    }

    #[test]
    fn shared_acquisition_times_never_predate_the_approval() {
        // Mia asked at :10, Noah's acquisition was submitted at :20, an
        // admin approved Mia's request at :30 (it attached then).
        let mut a = acquisition(A::Submitted);
        a.submitted_at = Some(t(20));
        let j = journey(&facts(Audience::Member, Some(approved_by_admin()), Some(a)));
        assert_eq!(j.steps[1].at, Some(t(30)));
        assert_eq!(j.steps[2].at, None);
        // Attached to an acquisition that was available already.
        let mut a = acquisition(A::Available);
        a.available_at = Some(t(25));
        let j = journey(&facts(Audience::Member, Some(approved_by_admin()), Some(a)));
        assert_eq!(j.steps[3].at, None);
    }

    #[test]
    fn members_never_see_names_or_raw_source_errors() {
        let mut declined = request(Decision::Rejected, Some(t(30)));
        declined.reject_reason = Some("Not now");
        let mut failed = acquisition(A::Failed);
        failed.last_error = Some("401 from radarr.internal");
        let mut stuck = acquisition(A::Downloading);
        stuck.progress = Some(1.0);
        stuck.source_message = Some("Sample file, not importing");
        let mut warn = acquisition(A::Downloading);
        warn.progress = Some(0.3);
        warn.source_message = Some("Stalled");
        for (request, acquisition) in [
            (declined, None),
            (approved_by_admin(), Some(failed)),
            (approved_by_admin(), Some(stuck)),
            (approved_by_admin(), Some(warn)),
            (approved_by_admin(), Some(acquisition(A::Submitted))),
        ] {
            let j = journey(&facts(Audience::Member, Some(request), acquisition));
            assert_eq!(j.source, None);
            for step in &j.steps {
                assert_eq!(step.actor, None, "{step:?}");
                let detail = step.detail.as_deref();
                assert!(detail.is_none() || detail == Some("Not now"), "{step:?}");
            }
        }
        let j = journey(&facts(
            Audience::Member,
            Some(approved_by_admin()),
            Some(failed),
        ));
        assert_eq!(j.steps[2].note, Some(Note::AdminTold));
        let j = journey(&facts(
            Audience::Member,
            Some(approved_by_admin()),
            Some(stuck),
        ));
        assert_eq!(j.steps[2].note, Some(Note::StuckAtImport));
        // A warning while still loading stays "downloading" for members.
        let j = journey(&facts(
            Audience::Member,
            Some(approved_by_admin()),
            Some(warn),
        ));
        assert_eq!((j.steps[2].step, j.steps[2].status), (Downloading, Current));
    }

    #[test]
    fn admins_see_held_names_and_the_sources_messages() {
        let mut held = acquisition(A::Pending);
        held.held = true;
        let j = journey(&facts(
            Audience::Admin,
            Some(approved_by_admin()),
            Some(held),
        ));
        assert_eq!(
            shape(&j),
            vec![
                (Requested, Done),
                (Approved, Done),
                (Held, Warning),
                (GettingIt, Upcoming),
                (Arriving, Upcoming),
                (Ready, Upcoming)
            ]
        );
        assert_eq!(j.steps[1].actor.as_deref(), Some("root"));
        assert_eq!(j.source.as_deref(), Some("Radarr"));

        // An admin's approval in the same instant is still the admin's.
        let j = journey(&facts(
            Audience::Admin,
            Some(request(Decision::Approved, Some(t(10)))),
            Some(acquisition(A::Pending)),
        ));
        assert_eq!(j.steps[1].step, Approved);
        // Approved automatically: no admin to name.
        let j = journey(&facts(
            Audience::Admin,
            Some(auto()),
            Some(acquisition(A::Pending)),
        ));
        assert_eq!(j.steps[1].step, ApprovedAutomatically);
        assert_eq!(j.steps[1].actor, None);

        let mut failed = acquisition(A::Failed);
        failed.last_error = Some("Radarr refused: exists");
        let j = journey(&facts(
            Audience::Admin,
            Some(approved_by_admin()),
            Some(failed),
        ));
        assert_eq!(j.steps[2].detail.as_deref(), Some("Radarr refused: exists"));
        assert_eq!(j.steps[2].note, None);

        let mut stuck = acquisition(A::Downloading);
        stuck.progress = Some(1.0);
        stuck.source_message = Some("Sample file");
        let j = journey(&facts(
            Audience::Admin,
            Some(approved_by_admin()),
            Some(stuck),
        ));
        assert_eq!((j.steps[2].step, j.steps[2].status), (Importing, Warning));
        assert_eq!(j.steps[2].detail.as_deref(), Some("Sample file"));

        let mut warn = acquisition(A::Downloading);
        warn.progress = Some(0.3);
        warn.source_message = Some("Stalled");
        let j = journey(&facts(
            Audience::Admin,
            Some(approved_by_admin()),
            Some(warn),
        ));
        assert_eq!((j.steps[2].step, j.steps[2].status), (Downloading, Warning));
        assert_eq!(j.steps[2].detail.as_deref(), Some("Stalled"));

        let mut declined = request(Decision::Rejected, Some(t(30)));
        declined.reject_reason = Some("No");
        let j = journey(&facts(Audience::Admin, Some(declined), None));
        assert_eq!(j.steps[1].actor.as_deref(), Some("root"));
        // No withdraw hint for admins.
        let j = journey(&facts(
            Audience::Admin,
            Some(request(Decision::PendingApproval, None)),
            None,
        ));
        assert_eq!(j.steps[1].note, None);
    }

    #[test]
    fn an_acquisition_on_its_own_starts_at_approval() {
        let mut a = acquisition(A::Downloading);
        a.stage = Some(DownloadStage::Arriving);
        a.stage_since = Some(t(40));
        let j = journey(&facts(Audience::Admin, None, Some(a)));
        assert_eq!(
            shape(&j),
            vec![
                (Approved, Done),
                (GettingIt, Done),
                (Arriving, Current),
                (Ready, Upcoming)
            ]
        );
        assert_eq!(j.steps[0].at, Some(t(20)));
        assert_eq!(j.steps[2].at, Some(t(40)));
        let mut held = acquisition(A::Pending);
        held.held = true;
        let j = journey(&facts(Audience::Admin, None, Some(held)));
        assert_eq!(j.steps[1], JourneyStep::new(Held, Warning));
    }
}
