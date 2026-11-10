//! Rendering notifications for channels outside Starr (lesson 30): the
//! content arrives structured and is turned into text only here, with
//! channel-correct escaping of every foreign value (titles, reasons,
//! usernames), structure-aware truncation, and no titles of adult content.
//! Links point at `STARR_PUBLIC_URL`; nothing is loaded from other hosts.

use std::fmt::Write as _;

use serde_json::{Value, json};
use starr_core::notifications::{NotificationKind, Payload};
use url::Url;

/// Longest title or reason shown, in characters.
const MAX_FIELD_CHARS: usize = 300;
/// Discord limits (embed title 256, description 4096).
const DISCORD_TITLE_CHARS: usize = 256;
const DISCORD_DESCRIPTION_CHARS: usize = 4096;
/// Polaris gold, the one accent (`docs/design`), as Discord embed color.
const POLARIS: u32 = 0x00F3_C46B;

/// One notification as words: a headline and a sentence or two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// Subject line / embed title.
    pub headline: String,
    /// Plain-text body (no markup).
    pub body: String,
    /// Where to look in Starr, if the public URL is known.
    pub link: Option<Url>,
}

/// An email: subject, plain text and a simple HTML alternative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// Cut `text` to at most `max` characters, ending with an ellipsis when cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Control characters (newlines in a title) would break subject lines and
/// layouts; they become spaces.
fn clean(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    truncate(flat.trim(), MAX_FIELD_CHARS)
}

/// The title as channels outside Starr may show it.
fn display_title(payload: &Payload) -> String {
    if !payload.title_shareable() {
        return "a title in a private category".to_owned();
    }
    let mut out = clean(&payload.title);
    if let Some(year) = payload.year {
        let _ = write!(out, " ({year})");
    }
    match payload.seasons.as_slice() {
        [] => {}
        [one] => {
            let _ = write!(out, ", season {one}");
        }
        many => {
            let list: Vec<String> = many.iter().map(u32::to_string).collect();
            let _ = write!(out, ", seasons {}", list.join(", "));
        }
    }
    out
}

fn link(public_url: Option<&Url>, kind: NotificationKind) -> Option<Url> {
    let path = match kind {
        NotificationKind::ApprovalPending
        | NotificationKind::AccountPrivileged
        | NotificationKind::AccountLocked => "admin/needs-you",
        NotificationKind::AcquisitionFailed | NotificationKind::ImportBlocked => "admin/requests",
        NotificationKind::AccountActivate => "services",
        _ => "requests",
    };
    public_url.and_then(|base| {
        let mut base = base.clone();
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        base.join(path).ok()
    })
}

/// The reason as channels may show it: it may name the title (an admin's
/// words, a service's error), so a private one only points to Starr.
fn shown_reason(payload: &Payload) -> Option<String> {
    if payload.title_shareable() {
        payload.reason.as_deref().map(clean)
    } else {
        payload
            .reason
            .as_ref()
            .map(|_| "details are in Starr".to_owned())
    }
}

/// Verb, followed unit and followed thing of a category: music is listened
/// to (spec 0006 C10, research #14), and its follows are artists bringing
/// albums (slice 6).
fn wording(category: &str) -> (&'static str, &'static str, &'static str) {
    if category == "music" {
        ("listen to", "album", "artist")
    } else {
        ("watch", "season", "series")
    }
}

/// Headline and body of what a follow did: requested something new, or
/// found more than the weekly cap allows (spec 0007 Q4). A private one
/// names nothing (Q8).
fn follow_text(
    kind: NotificationKind,
    title: &str,
    private: bool,
    category: &str,
) -> (String, String) {
    let (_, unit, followed) = wording(category);
    match (kind, private) {
        (NotificationKind::FollowCapped, true) => (
            "More new titles than your weekly limit".to_owned(),
            "A follow in a private category has more new scenes than your weekly limit. \
             Open Starr to request them yourself."
                .to_owned(),
        ),
        // Only private follows are capped today; a shareable one may name it.
        (NotificationKind::FollowCapped, false) => (
            "More new titles than your weekly limit".to_owned(),
            format!(
                "{title} has more new titles than your weekly limit. Open Starr to request them yourself."
            ),
        ),
        (_, true) => (
            "New title requested".to_owned(),
            format!("Starr requested {title} for you, because you follow it."),
        ),
        (_, false) => (
            format!("New {unit} requested"),
            format!("Starr requested {title} for you, because you follow the {followed}."),
        ),
    }
}

/// Headline and body of a notification, in plain text.
#[must_use]
pub fn render(kind: NotificationKind, payload: &Payload, public_url: Option<&Url>) -> Rendered {
    let title = display_title(payload);
    let reason = shown_reason(payload);
    // A private notification names nothing outside Starr (spec 0007 Q8):
    // no title, year, seasons, reason or member; only kind and category.
    let private = payload.is_private();
    let shown: std::borrow::Cow<'_, Payload> = if private {
        std::borrow::Cow::Owned(payload.neutral())
    } else {
        std::borrow::Cow::Borrowed(payload)
    };
    let payload: &Payload = &shown;
    let (verb, _, _) = wording(&payload.category);
    let (headline, body) = match kind {
        NotificationKind::RequestApproved => (
            "Request approved".to_owned(),
            format!("Your request for {title} was approved. Starr is fetching it now."),
        ),
        NotificationKind::RequestRejected => (
            "Request declined".to_owned(),
            match reason {
                Some(reason) => format!("Your request for {title} was declined. Reason: {reason}"),
                None => format!("Your request for {title} was declined."),
            },
        ),
        NotificationKind::RequestRevoked => (
            "Request revoked".to_owned(),
            match reason {
                Some(reason) => {
                    format!("An admin revoked your request for {title}. Reason: {reason}")
                }
                None => format!("An admin revoked your request for {title}."),
            },
        ),
        NotificationKind::RequestPartiallyAvailable => (
            format!("Partly ready to {verb}"),
            format!("The first part of {title} is ready to {verb}. The rest follows."),
        ),
        NotificationKind::RequestAvailable => (
            format!("Ready to {verb}"),
            format!("{title} is ready to {verb}."),
        ),
        NotificationKind::FollowRequested | NotificationKind::FollowCapped => {
            follow_text(kind, &title, private, &payload.category)
        }
        NotificationKind::AccountActivate => (
            "Activate your account".to_owned(),
            format!(
                "Starr needs your password once to set up your account in {title}. Open My services and activate it."
            ),
        ),
        NotificationKind::RequestFailed => (
            "Request stuck".to_owned(),
            format!("Fetching {title} ran into trouble. An admin has been told."),
        ),
        NotificationKind::ApprovalPending => (
            "Needs you: approval".to_owned(),
            match payload.member.as_deref().map(clean) {
                Some(member) => format!("{member} asked for {title}. It waits for your approval."),
                None => format!("A request for {title} waits for your approval."),
            },
        ),
        NotificationKind::AcquisitionFailed => (
            "Acquisition failed".to_owned(),
            match reason {
                Some(reason) => format!("Fetching {title} failed: {reason}"),
                None => format!("Fetching {title} failed."),
            },
        ),
        NotificationKind::ImportBlocked => (
            "Import blocked".to_owned(),
            match reason {
                Some(reason) => {
                    format!("{title} is downloaded, but the source does not import it: {reason}")
                }
                None => format!("{title} is downloaded, but the source does not import it."),
            },
        ),
        NotificationKind::AccountPrivileged => (
            "Needs you: account with admin rights".to_owned(),
            format!(
                "{} has administrator rights in {title} now. Starr keeps its hands off that account until you look.",
                member_or_someone(payload)
            ),
        ),
        NotificationKind::AccountLocked => (
            "Account locked".to_owned(),
            format!(
                "{title} locked the account of {} after failed sign-ins. Starr never lifts that; unlock it in {title} if it is fine.",
                member_or_someone(payload)
            ),
        ),
    };
    Rendered {
        headline,
        body,
        link: link(public_url, kind),
    }
}

fn member_or_someone(payload: &Payload) -> String {
    payload
        .member
        .as_deref()
        .map_or_else(|| "A member".to_owned(), clean)
}

/// The test message of a channel.
#[must_use]
pub fn test_message(public_url: Option<&Url>) -> Rendered {
    Rendered {
        headline: "Test from Starr".to_owned(),
        body: "This channel works. Notifications will arrive here.".to_owned(),
        link: public_url.cloned(),
    }
}

/// Escape text for HTML element content and attribute values.
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// An email in the Nachtaufnahme look: night ground, starlight text, one
/// Polaris-gold link. Inline styles only, no images, no foreign hosts.
#[must_use]
pub fn email(rendered: &Rendered) -> Email {
    let subject = format!("Starr · {}", rendered.headline);
    let mut text = format!("{}\n\n{}\n", rendered.headline, rendered.body);
    if let Some(link) = &rendered.link {
        let _ = writeln!(text, "\nOpen Starr: {link}");
    }
    text.push_str("\n—\nStarr. You can change which emails you get in your profile.\n");
    let button = rendered.link.as_ref().map_or_else(String::new, |link| {
        format!(
            "<p style=\"margin:28px 0 0\"><a href=\"{}\" style=\"display:inline-block;padding:10px 20px;border-radius:999px;background:#F3C46B;color:#1D1505;font-weight:600;text-decoration:none\">Open Starr</a></p>",
            html_escape(link.as_str())
        )
    });
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{subject}</title></head>\
<body style=\"margin:0;padding:0;background:#0B0F15\">\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#0B0F15\"><tr><td align=\"center\" style=\"padding:32px 16px\">\
<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:520px;background:#121821;border:1px solid #263042;border-radius:16px\"><tr><td style=\"padding:28px 28px 32px;font-family:'Instrument Sans',Helvetica,Arial,sans-serif;color:#E9EDF2;font-size:15px;line-height:1.55\">\
<p style=\"margin:0 0 20px;font-weight:800;font-size:18px;letter-spacing:-0.03em;color:#E9EDF2\">starr</p>\
<h1 style=\"margin:0 0 12px;font-size:21px;line-height:1.3;font-weight:700;color:#E9EDF2\">{headline}</h1>\
<p style=\"margin:0;color:#9AA6B8\">{body}</p>{button}\
</td></tr></table>\
<p style=\"margin:20px 0 0;font-family:Helvetica,Arial,sans-serif;font-size:12px;color:#7E8BA1\">You can change which emails you get in your Starr profile.</p>\
</td></tr></table></body></html>",
        subject = html_escape(&subject),
        headline = html_escape(&rendered.headline),
        body = html_escape(&rendered.body),
    );
    Email {
        subject,
        text,
        html,
    }
}

/// Escape Discord markdown so foreign text shows literally.
fn discord_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*'
                | '_'
                | '~'
                | '`'
                | '|'
                | '>'
                | '#'
                | '['
                | ']'
                | '('
                | ')'
                | '-'
                | '<'
                | '@'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A Discord webhook message: one embed, mentions disabled.
#[must_use]
pub fn discord(rendered: &Rendered) -> Value {
    let mut embed = json!({
        "title": truncate(&discord_escape(&rendered.headline), DISCORD_TITLE_CHARS),
        "description": truncate(&discord_escape(&rendered.body), DISCORD_DESCRIPTION_CHARS),
        "color": POLARIS,
    });
    if let Some(link) = &rendered.link {
        embed["url"] = Value::String(link.to_string());
    }
    json!({
        "username": "Starr",
        "embeds": [embed],
        // Nobody gets pinged by a title like "@everyone".
        "allowed_mentions": { "parse": [] },
    })
}

/// What a webhook calls a private category (ADR 0014, maintainer
/// 2026-11-06): the receiver learns that it is private, not which one.
const PRIVATE_CATEGORY: &str = "private";

/// A generic webhook body: the structured event plus the rendered text.
#[must_use]
pub fn webhook(
    kind: Option<NotificationKind>,
    payload: Option<&Payload>,
    rendered: &Rendered,
    created_at: &str,
) -> Value {
    let mut data = payload.map_or(Value::Null, |p| {
        // Private: only kind, category and ids (spec 0007 Q8), and the
        // category named `private`.
        let shown = if p.is_private() {
            Payload {
                category: PRIVATE_CATEGORY.to_owned(),
                ..p.neutral()
            }
        } else {
            p.clone()
        };
        serde_json::to_value(shown).unwrap_or(Value::Null)
    });
    if let Value::Object(map) = &mut data
        && map.get("title").and_then(Value::as_str) == Some("")
    {
        map.remove("title");
    }
    json!({
        "event": kind.map_or("test", NotificationKind::as_str),
        "createdAt": created_at,
        "headline": rendered.headline,
        "text": rendered.body,
        "url": rendered.link.as_ref().map(Url::as_str),
        "data": data,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn payload(title: &str) -> Payload {
        Payload {
            title: title.to_owned(),
            year: Some(1999),
            category: "movies".into(),
            request_id: Some(7),
            ..Payload::default()
        }
    }

    fn public() -> Url {
        Url::parse("https://starr.example.org/base").unwrap()
    }

    #[test]
    fn every_kind_renders_with_title_and_link() {
        for kind in NotificationKind::ALL {
            let r = render(kind, &payload("The Matrix"), Some(&public()));
            assert!(r.body.contains("The Matrix (1999)"), "{kind:?}: {}", r.body);
            assert!(r.headline.len() > 3, "{kind:?}");
            assert!(
                r.link
                    .as_ref()
                    .unwrap()
                    .as_str()
                    .starts_with("https://starr.example.org/base/"),
                "{kind:?}"
            );
        }
        let r = render(NotificationKind::RequestAvailable, &payload("X"), None);
        assert_eq!(r.link, None);
    }

    #[test]
    fn seasons_reasons_and_members_are_named() {
        let mut p = payload("Dark");
        p.seasons = vec![1, 2];
        p.reason = Some("not in 4K".into());
        p.member = Some("mia".into());
        let rejected = render(NotificationKind::RequestRejected, &p, None);
        assert_eq!(
            rejected.body,
            "Your request for Dark (1999), seasons 1, 2 was declined. Reason: not in 4K"
        );
        let pending = render(NotificationKind::ApprovalPending, &p, None);
        assert!(pending.body.starts_with("mia asked for Dark"));
        let revoked = render(NotificationKind::RequestRevoked, &p, None);
        assert_eq!(
            revoked.body,
            "An admin revoked your request for Dark (1999), seasons 1, 2. Reason: not in 4K"
        );
    }

    #[test]
    fn music_is_ready_to_listen_to() {
        let mut p = payload("Radiohead – OK Computer");
        p.year = Some(1997);
        p.category = "music".into();
        let ready = render(NotificationKind::RequestAvailable, &p, None);
        assert_eq!(ready.headline, "Ready to listen to");
        assert_eq!(
            ready.body,
            "Radiohead – OK Computer (1997) is ready to listen to."
        );
        let partly = render(NotificationKind::RequestPartiallyAvailable, &p, None);
        assert_eq!(partly.headline, "Partly ready to listen to");
        let movie = render(NotificationKind::RequestAvailable, &payload("Heat"), None);
        assert_eq!(movie.headline, "Ready to watch");
    }

    #[test]
    fn a_followed_artist_brings_a_new_album() {
        let mut p = payload("Radiohead – Next Album");
        p.year = Some(2027);
        p.category = "music".into();
        let found = render(NotificationKind::FollowRequested, &p, None);
        assert_eq!(found.headline, "New album requested");
        assert_eq!(
            found.body,
            "Starr requested Radiohead – Next Album (2027) for you, because you follow the artist."
        );
        let series = render(NotificationKind::FollowRequested, &payload("Dark"), None);
        assert_eq!(series.headline, "New season requested");
        assert!(series.body.ends_with("because you follow the series."));
    }

    #[test]
    fn adult_titles_are_not_named() {
        let mut p = payload("Secret Title");
        p.category = "adult".into();
        p.reason = Some("Secret Title is not wanted here".into());
        for kind in NotificationKind::ALL {
            let r = render(kind, &p, None);
            assert!(!r.body.contains("Secret Title"), "{kind:?}");
            let body = webhook(Some(kind), Some(&p), &r, "t");
            assert!(!body.to_string().contains("Secret Title"), "{kind:?}");
            assert!(
                !discord(&r).to_string().contains("Secret Title"),
                "{kind:?}"
            );
            assert!(!email(&r).html.contains("Secret Title"), "{kind:?}");
        }
        let rejected = render(NotificationKind::RequestRejected, &p, None);
        assert!(rejected.body.contains("details are in Starr"));
    }

    /// Spec 0007 acceptance: for every kind of an adult notification, the
    /// email (subject, text, HTML), the Discord embed and the webhook JSON
    /// carry no title, name, year, seasons, reason, member, image or
    /// `StashDB` UUID; links never point at a title.
    #[test]
    fn adult_notifications_are_neutral_on_every_channel() {
        const UUID: &str = "7f3c1a52-0d4e-4b8a-9c61-2a5e8f9b0c11";
        let p = Payload {
            title: format!("Scene A by Performer P stashdb:{UUID}"),
            year: Some(1987),
            category: "adult".into(),
            request_id: Some(7),
            acquisition_id: Some(9),
            seasons: vec![4711],
            reason: Some("Studio S refused".into()),
            member: Some("ada-the-member".into()),
        };
        let markers = [
            "Scene A",
            "Performer P",
            "Studio S",
            "1987",
            "4711",
            "ada-the-member",
            UUID,
            "stashdb",
            "/images/",
            // The category itself is `private` outside Starr (ADR 0014,
            // maintainer 2026-11-06).
            "adult",
        ];
        for kind in NotificationKind::ALL {
            let r = render(kind, &p, Some(&public()));
            let mail = email(&r);
            let structured = webhook(Some(kind), Some(&p), &r, "t");
            assert_eq!(structured["data"]["category"], "private", "{kind:?}");
            let webhook = structured.to_string();
            let discord = discord(&r).to_string();
            for text in [
                &r.headline,
                &r.body,
                &mail.subject,
                &mail.text,
                &mail.html,
                &webhook,
                &discord,
            ] {
                for marker in markers {
                    assert!(!text.contains(marker), "{kind:?} names {marker}: {text}");
                }
            }
            let link = r.link.unwrap();
            assert!(
                [
                    "/base/requests",
                    "/base/admin/needs-you",
                    "/base/admin/requests",
                    "/base/services"
                ]
                .contains(&link.path()),
                "{kind:?}: {link}"
            );
        }
        // Members are told where to look, without a deep link.
        let ready = render(NotificationKind::RequestAvailable, &p, Some(&public()));
        assert_eq!(ready.link.unwrap().path(), "/base/requests");
        let followed = render(NotificationKind::FollowRequested, &p, None);
        assert!(!followed.body.contains("series"), "{}", followed.body);
        // The weekly cap (spec 0007 Q4) in the words of the spec.
        let capped = render(NotificationKind::FollowCapped, &p, Some(&public()));
        assert_eq!(
            capped.body,
            "A follow in a private category has more new scenes than your weekly limit. \
             Open Starr to request them yourself."
        );
        assert_eq!(capped.link.unwrap().path(), "/base/requests");
    }

    /// Lesson 30: hostile titles stay text in every channel.
    #[test]
    fn hostile_titles_are_escaped_and_truncated() {
        let hostile = "<b>Bold</b> `code` @everyone **x** \"q\" 🎬\nnext";
        let r = render(
            NotificationKind::RequestAvailable,
            &payload(hostile),
            Some(&public()),
        );
        assert!(!r.body.contains('\n'), "control characters become spaces");

        let mail = email(&r);
        assert!(!mail.html.contains("<b>Bold"));
        assert!(mail.html.contains("&lt;b&gt;Bold&lt;/b&gt;"));
        assert!(mail.html.contains("&quot;q&quot;"));
        assert!(mail.html.contains("🎬"));
        assert!(
            mail.text.contains("<b>Bold</b>"),
            "plain text stays literal"
        );
        assert!(!mail.subject.contains('\n'));
        assert!(
            !mail.html.contains("http://"),
            "no foreign or insecure hosts"
        );
        assert!(!mail.html.contains("<img"));

        let d = discord(&r);
        let description = d["embeds"][0]["description"].as_str().unwrap();
        assert!(description.contains("\\`code\\`"));
        assert!(description.contains("\\@everyone"));
        assert!(description.contains("\\*\\*x\\*\\*"));
        assert_eq!(d["allowed_mentions"]["parse"], json!([]));

        let long = "x".repeat(10_000);
        let r = render(NotificationKind::RequestAvailable, &payload(&long), None);
        assert!(r.body.chars().count() < 400);
        let d = discord(&Rendered {
            headline: long.clone(),
            body: long,
            link: None,
        });
        let title = d["embeds"][0]["title"].as_str().unwrap();
        assert_eq!(title.chars().count(), DISCORD_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn blocked_imports_carry_the_sources_message_except_for_adult_titles() {
        let mut p = payload("The Matrix");
        p.acquisition_id = Some(3);
        p.reason = Some("Not an upgrade for existing movie file(s)".into());
        let r = render(NotificationKind::ImportBlocked, &p, Some(&public()));
        assert_eq!(r.headline, "Import blocked");
        assert_eq!(
            r.body,
            "The Matrix (1999) is downloaded, but the source does not import it: \
             Not an upgrade for existing movie file(s)"
        );
        assert_eq!(
            r.link.as_ref().unwrap().as_str(),
            "https://starr.example.org/base/admin/requests"
        );
        let body = webhook(Some(NotificationKind::ImportBlocked), Some(&p), &r, "t");
        assert_eq!(body["event"], "import_blocked");
        assert_eq!(body["data"]["acquisitionId"], 3);
        let mail = email(&r);
        assert_eq!(mail.subject, "Starr · Import blocked");
        let d = discord(&r);
        assert_eq!(d["allowed_mentions"]["parse"], json!([]));

        // The source's message names the release, so the title too.
        p.category = "adult".into();
        p.reason = Some("Secret.Title.2024.1080p not imported".into());
        let r = render(NotificationKind::ImportBlocked, &p, None);
        assert!(!r.body.contains("Secret"), "{}", r.body);
        assert!(r.body.contains("details are in Starr"));
    }

    #[test]
    fn webhook_carries_the_structured_event() {
        let p = payload("The Matrix");
        let r = render(NotificationKind::RequestApproved, &p, None);
        let body = webhook(
            Some(NotificationKind::RequestApproved),
            Some(&p),
            &r,
            "2026-11-02T10:00:00.000Z",
        );
        assert_eq!(body["event"], "request_approved");
        assert_eq!(body["data"]["title"], "The Matrix");
        assert_eq!(body["data"]["requestId"], 7);
        let test = webhook(None, None, &test_message(None), "t");
        assert_eq!(test["event"], "test");
    }
}
