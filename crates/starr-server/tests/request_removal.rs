//! Removing requests (spec 0004): members cancel and hide their own,
//! admins revoke in bulk with a reason. Two members against one title
//! (lesson 4).
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use common::media::{put, setup};
use common::{get, post};
use serde_json::{Value, json};

fn db(s: &common::media::Setup) -> &starr_db::Database {
    &s.env.app.state.db
}

async fn audit_actions(s: &common::media::Setup) -> Vec<String> {
    db(s)
        .recent_audit(50)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.action)
        .collect()
}

fn revoke(requests: &[(i64, i64)], reason: &str) -> Value {
    json!({
        "requests": requests
            .iter()
            .map(|(id, member)| json!({ "id": id, "memberId": member }))
            .collect::<Vec<_>>(),
        "reason": reason,
    })
}

#[tokio::test]
async fn members_cancel_and_hide_only_their_own_requests() {
    let s = setup().await;
    // Noah requests automatically: approved, the acquisition waits.
    let noah = s.request_movie(&s.noah).await;
    assert_eq!(noah.status, StatusCode::CREATED, "{}", noah.raw);
    assert_eq!(noah.body["request"]["canCancel"], true);
    assert_eq!(noah.body["request"]["canHide"], false);
    let id = noah.body["request"]["id"].as_i64().unwrap();
    let cancel = format!("/api/v1/requests/{id}/cancel");
    let hidden = format!("/api/v1/requests/{id}/hidden");

    // Mia cannot cancel or hide Noah's request, nor learn it exists.
    let res = s.as_member(&s.mia, post(&cancel, json!({}))).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "requests.not_found");
    // Not finished yet: nothing to hide.
    let res = s
        .as_member(&s.noah, put(&hidden, json!({ "hidden": true })))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.not_finished");

    let res = s.as_member(&s.noah, post(&cancel, json!({}))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["state"], "cancelled");
    assert_eq!(res.body["canCancel"], false);
    assert_eq!(res.body["canHide"], true);
    let steps = res.body["items"][0]["journey"]["steps"].as_array().unwrap();
    assert_eq!(steps.last().unwrap()["step"], "cancelled");
    assert_eq!(steps.last().unwrap()["status"], "ended");
    let res = s.as_member(&s.noah, post(&cancel, json!({}))).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.not_cancellable");
    // Nobody wanted it before it went out: cancelled, nothing to stop.
    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["state"], "cancelled");
    assert_eq!(acquisitions.body[0]["stopPending"], false);

    let res = s
        .as_member(&s.mia, put(&hidden, json!({ "hidden": true })))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = s
        .as_member(&s.noah, put(&hidden, json!({ "hidden": true })))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["hidden"], true);
    let list = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(list.body, json!([]));
    let all = s
        .as_member(&s.noah, get("/api/v1/requests?hidden=true"))
        .await;
    assert_eq!(all.body.as_array().unwrap().len(), 1);
    // Admins still see it, marked hidden.
    let admin = s.admin(get("/api/v1/admin/requests")).await;
    assert_eq!(admin.body[0]["id"], id);
    assert_eq!(admin.body[0]["hidden"], true);
    assert_eq!(admin.body[0]["canRevoke"], false);

    let res = s
        .as_member(&s.noah, put(&hidden, json!({ "hidden": false })))
        .await;
    assert_eq!(res.body["hidden"], false);
    let actions = audit_actions(&s).await;
    for action in ["request.cancel", "request.hide", "request.unhide"] {
        assert!(actions.iter().any(|a| a == action), "{action}: {actions:?}");
    }

    // After an own cancel the normal mode applies again.
    let again = s.request_movie(&s.noah).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.raw);
    assert_eq!(again.body["request"]["state"], "approved");
}

#[tokio::test]
async fn a_running_download_cannot_be_cancelled_by_the_member() {
    let s = setup().await;
    let noah = s.request_movie(&s.noah).await;
    let id = noah.body["request"]["id"].as_i64().unwrap();
    sqlx::query("UPDATE acquisition SET state = 'downloading'")
        .execute(db(&s).pool())
        .await
        .unwrap();
    let list = s.as_member(&s.noah, get("/api/v1/requests")).await;
    assert_eq!(list.body[0]["canCancel"], false);
    let res = s
        .as_member(
            &s.noah,
            post(&format!("/api/v1/requests/{id}/cancel"), json!({})),
        )
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.not_cancellable");
}

#[tokio::test]
async fn admins_revoke_in_bulk_with_a_reason_the_member_reads() {
    let s = setup().await;
    let movie = s.request_movie(&s.noah).await;
    let series = s.request_seasons(&s.noah, &[1]).await;
    // Mia asks too: she attaches to the same acquisition.
    assert_eq!(s.request_movie(&s.mia).await.status, StatusCode::CREATED);
    let movie_id = movie.body["request"]["id"].as_i64().unwrap();
    let series_id = series.body["request"]["id"].as_i64().unwrap();
    let path = "/api/v1/admin/requests/revoke";

    // Members cannot revoke.
    let res = s
        .as_member(&s.noah, post(path, revoke(&[(movie_id, s.noah.id)], "x")))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    // A reason is required, the selection must not be empty.
    let res = s
        .admin(post(path, revoke(&[(movie_id, s.noah.id)], " ")))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "requests.reason_invalid");
    let res = s.admin(post(path, revoke(&[], "x"))).await;
    assert_eq!(res.code(), "requests.selection_invalid");
    // Each request names its owner (lesson 4).
    let res = s
        .admin(post(path, revoke(&[(movie_id, s.mia.id)], "x")))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "requests.owner_mismatch");

    let res = s
        .admin(post(
            path,
            revoke(
                &[(movie_id, s.noah.id), (series_id, s.noah.id)],
                "not on this server",
            ),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["revoked"], json!([movie_id, series_id]));

    let mine = s.as_member(&s.noah, get("/api/v1/requests")).await;
    for request in mine.body.as_array().unwrap() {
        assert_eq!(request["state"], "revoked");
        assert_eq!(request["revokeReason"], "not on this server");
        let step = request["items"][0]["journey"]["steps"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(step["step"], "revoked");
        assert_eq!(step["status"], "failed");
        assert_eq!(step["detail"], "not on this server");
        // Members never see who revoked it.
        assert_eq!(step["actor"], Value::Null);
    }
    // Mia attached to the same acquisition: untouched, still wanted.
    let theirs = s.as_member(&s.mia, get("/api/v1/requests")).await;
    assert_eq!(theirs.body[0]["state"], "approved");
    assert!(!theirs.raw.contains("not on this server"));

    let notes = s.as_member(&s.noah, get("/api/v1/notifications")).await;
    let revoked: Vec<&Value> = notes.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "request_revoked")
        .collect();
    assert_eq!(revoked.len(), 2, "{}", notes.raw);
    assert_eq!(revoked[0]["reason"], "not on this server");

    // Admins see who did it.
    let detail = s
        .admin(get(&format!("/api/v1/admin/requests/{movie_id}")))
        .await;
    let step = detail.body["request"]["items"][0]["journey"]["steps"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(step["actor"], common::ADMIN_USER);
    assert!(
        audit_actions(&s)
            .await
            .iter()
            .any(|a| a == "requests.revoke")
    );

    // Revoking again: nothing open, skipped.
    let res = s
        .admin(post(path, revoke(&[(movie_id, s.noah.id)], "again")))
        .await;
    assert_eq!(res.body["skipped"], json!([movie_id]));

    // Noah's next request for the movie waits for approval despite auto.
    let again = s.request_movie(&s.noah).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.raw);
    assert_eq!(again.body["request"]["state"], "pending_approval");
}

#[tokio::test]
async fn a_stop_held_by_paused_writes_is_listed_for_the_resume() {
    let s = setup().await;
    let noah = s.request_movie(&s.noah).await;
    let id = noah.body["request"]["id"].as_i64().unwrap();
    // Submitted, and Starr switched the monitoring on.
    sqlx::query(
        "UPDATE acquisition SET state = 'submitted', submitted_at = created_at,
             starr_monitored = 1",
    )
    .execute(db(&s).pool())
    .await
    .unwrap();
    let res = s
        .admin(put(
            &format!("/api/v1/services/{}/writes-paused", s.radarr),
            json!({ "paused": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = s
        .as_member(
            &s.noah,
            post(&format!("/api/v1/requests/{id}/cancel"), json!({})),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    let held = s
        .admin(get(&format!("/api/v1/services/{}/held", s.radarr)))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.raw);
    assert_eq!(held.body["total"], 0);
    assert_eq!(held.body["stopTotal"], 1);
    assert_eq!(held.body["stops"][0]["title"], "Inception");

    let acquisitions = s.admin(get("/api/v1/admin/acquisitions")).await;
    assert_eq!(acquisitions.body[0]["state"], "submitted");
    assert_eq!(acquisitions.body[0]["unwanted"], true);
    assert_eq!(acquisitions.body[0]["stopPending"], true);
    assert_eq!(acquisitions.body[0]["stopHeld"], true);
    assert_eq!(acquisitions.body[0]["requests"], 0);
}
