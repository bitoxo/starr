//! The admin attention model (spec 0008 F9): one list, one count, for the
//! Inbox, its badge, the title and the bell; approvals undone while nothing
//! went to the source (F-4).
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::media::{put, setup};
use common::services::delete;
use common::{get, post};
use serde_json::{Value, json};

fn refs(requests: &[(i64, i64)]) -> Value {
    json!({
        "requests": requests
            .iter()
            .map(|(id, member)| json!({ "id": id, "memberId": member }))
            .collect::<Vec<_>>()
    })
}

fn kinds(attention: &Value) -> Vec<String> {
    attention["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// The one count: total, the section counters and the items agree.
fn assert_counts_agree(attention: &Value) {
    let items = attention["items"].as_array().unwrap();
    assert_eq!(attention["total"], items.len(), "{attention}");
    let sections = &attention["sections"];
    let sum: u64 = ["decide", "fix", "paused", "setup"]
        .iter()
        .map(|s| sections[s].as_u64().unwrap())
        .sum();
    assert_eq!(sum, items.len() as u64, "{attention}");
    for section in ["decide", "fix", "paused", "setup"] {
        let n = items.iter().filter(|i| i["section"] == section).count();
        assert_eq!(sections[section], n, "{section}: {attention}");
    }
}

#[tokio::test]
async fn approvals_held_requests_and_setup_are_counted_once() {
    let s = setup().await;
    let attention = s.admin(get("/api/v1/admin/attention")).await;
    assert_eq!(attention.status, StatusCode::OK, "{}", attention.raw);
    // Services, categories and members are set up; two-step verification
    // for this admin is not.
    assert_eq!(kinds(&attention.body), vec!["setup"]);
    let steps = &attention.body["items"][0]["setup"];
    assert_eq!(
        steps,
        &json!([
            { "step": "services", "done": true },
            { "step": "categories", "done": true },
            { "step": "two_step", "done": false },
            { "step": "members", "done": true },
        ])
    );

    // Mia asks with approval; Radarr's writes are paused and Noah's
    // automatic request waits there.
    let mia = s.request_movie(&s.mia).await;
    let mia_id = mia.body["request"]["id"].as_i64().unwrap();
    s.admin(put(
        &format!("/api/v1/services/{}/writes-paused", s.radarr),
        json!({ "paused": true }),
    ))
    .await;
    s.request_seasons(&s.noah, &[1]).await;
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);

    let attention = s.admin(get("/api/v1/admin/attention")).await;
    assert_counts_agree(&attention.body);
    assert_eq!(kinds(&attention.body), vec!["approval", "held", "setup"]);
    let approval = &attention.body["items"][0];
    assert_eq!(approval["id"], format!("approval:{mia_id}"));
    assert_eq!(approval["section"], "decide");
    assert_eq!(approval["request"]["title"], "Inception");
    assert_eq!(approval["redacted"], false);
    let held = &attention.body["items"][1];
    assert_eq!(held["id"], format!("held:{}", s.radarr));
    assert_eq!(held["section"], "paused");
    assert_eq!(held["writes"]["kind"], "source");
    assert_eq!(held["writes"]["serviceName"], "radarr");
    assert_eq!(held["writes"]["pausedBy"], common::ADMIN_USER);
    assert_eq!(held["writes"]["held"]["total"], 1);
    assert_eq!(held["writes"]["held"]["preview"][0]["title"], "Inception");
    assert_eq!(attention.body["sections"]["decide"], 1);
    assert_eq!(attention.body["sections"]["paused"], 1);

    // Members never see it.
    let res = s.as_member(&s.mia, get("/api/v1/admin/attention")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);

    // Resumed with nothing held: the paused item is gone.
    s.admin(put(
        &format!("/api/v1/services/{}/writes-paused", s.radarr),
        json!({ "paused": false }),
    ))
    .await;
    let attention = s.admin(get("/api/v1/admin/attention")).await;
    assert_eq!(kinds(&attention.body), vec!["approval", "setup"]);
}

#[tokio::test]
async fn failed_and_stuck_downloads_and_closed_categories_need_a_fix() {
    let s = setup().await;
    s.request_movie(&s.noah).await;
    s.request_seasons(&s.noah, &[1, 2]).await;
    let pool = s.env.app.state.db.pool();
    let ids: Vec<(i64, Option<i64>)> =
        sqlx::query_as("SELECT a.id, c.season_number FROM acquisition a JOIN catalog_item c ON c.id = a.catalog_item_id ORDER BY a.id")
            .fetch_all(pool)
            .await
            .unwrap();
    let season = ids.iter().find(|(_, s)| s == &Some(1)).unwrap().0;
    let other = ids.iter().find(|(_, s)| s == &Some(2)).unwrap().0;
    sqlx::query(
        "UPDATE acquisition SET state = 'failed', last_error = 'Path not allowed' WHERE id = ?",
    )
    .bind(season)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE acquisition SET state = 'downloading', progress = 1.0,
             source_message = 'No files found are eligible for import' WHERE id = ?",
    )
    .bind(other)
    .execute(pool)
    .await
    .unwrap();
    let attention = s.admin(get("/api/v1/admin/attention")).await;
    assert_counts_agree(&attention.body);
    let fix: Vec<&Value> = attention.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["section"] == "fix")
        .collect();
    assert_eq!(fix.len(), 2, "{}", attention.raw);
    let failed = fix.iter().find(|i| i["kind"] == "failed").unwrap();
    assert_eq!(failed["id"], format!("failed:{season}"));
    assert_eq!(failed["acquisition"]["lastError"], "Path not allowed");
    let stuck = fix.iter().find(|i| i["kind"] == "stuck").unwrap();
    assert_eq!(stuck["id"], format!("stuck:{other}"));

    // Removing the movie source closes Movies and fails its download.
    let res = s
        .admin(delete(&format!("/api/v1/services/{}", s.radarr)))
        .await;
    assert!(res.status.is_success(), "{}", res.raw);
    let attention = s.admin(get("/api/v1/admin/attention")).await;
    assert_counts_agree(&attention.body);
    let closed: Vec<&Value> = attention.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "category_closed")
        .collect();
    assert_eq!(closed.len(), 1, "{}", attention.raw);
    assert_eq!(closed[0]["id"], "category:movies");
    assert_eq!(closed[0]["category"]["category"], "movies");
    assert_eq!(closed[0]["category"]["sourceName"], Value::Null);
    assert_eq!(
        attention.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "failed")
            .count(),
        2
    );
}

#[tokio::test]
async fn approvals_held_at_a_paused_source_can_be_undone() {
    let s = setup().await;
    s.admin(put(
        &format!("/api/v1/services/{}/writes-paused", s.radarr),
        json!({ "paused": true }),
    ))
    .await;
    let mia = s.request_movie(&s.mia).await;
    let id = mia.body["request"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/approve",
            refs(&[(id, s.mia.id)]),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["decided"], json!([id]));
    // The toast says it waits: "Approved: Inception – held until radarr
    // resumes", with Undo.
    assert_eq!(res.body["undoable"], true);
    assert_eq!(
        res.body["held"],
        json!([{ "requestId": id, "source": "radarr" }])
    );

    let res = s
        .admin(post(
            "/api/v1/admin/approvals/undo",
            refs(&[(id, s.mia.id)]),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["decided"], json!([id]));
    let attention = s.admin(get("/api/v1/admin/attention")).await;
    assert!(kinds(&attention.body).contains(&"approval".to_owned()));
    let mine = s.as_member(&s.mia, get("/api/v1/requests")).await;
    assert_eq!(mine.body[0]["state"], "pending_approval");
    let notes = s.as_member(&s.mia, get("/api/v1/notifications")).await;
    assert!(
        notes.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["kind"] != "request_approved"),
        "{}",
        notes.raw
    );
    let audit = s.env.app.state.db.recent_audit(5).await.unwrap();
    assert_eq!(audit[0].action, "requests.approve_undone");
}

#[tokio::test]
async fn approvals_that_go_out_cannot_be_undone_and_rejections_never() {
    let s = setup().await;
    let mia = s.request_movie(&s.mia).await;
    let id = mia.body["request"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/approve",
            refs(&[(id, s.mia.id)]),
        ))
        .await;
    assert_eq!(res.body["undoable"], false, "{}", res.raw);
    assert_eq!(res.body["held"], json!([]));
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/undo",
            refs(&[(id, s.mia.id)]),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "requests.undo_too_late");
    let mine = s.as_member(&s.mia, get("/api/v1/requests")).await;
    assert_eq!(mine.body[0]["state"], "approved");

    // A rejection has nothing to undo.
    let series = s.request_seasons(&s.mia, &[1]).await;
    let sid = series.body["request"]["id"].as_i64().unwrap();
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/reject",
            json!({ "requests": [{ "id": sid, "memberId": s.mia.id }], "reason": "Not now" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["undoable"], false);
    let res = s
        .admin(post(
            "/api/v1/admin/approvals/undo",
            refs(&[(sid, s.mia.id)]),
        ))
        .await;
    assert_eq!(res.code(), "requests.undo_too_late");
    // Members cannot undo.
    let res = s
        .as_member(
            &s.mia,
            post("/api/v1/admin/approvals/undo", refs(&[(id, s.mia.id)])),
        )
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}
