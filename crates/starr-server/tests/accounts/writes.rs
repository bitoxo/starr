//! Account writes with safe mode (spec 0005 slice 4) through the API:
//! every instance starts plan only; going live, resuming and approving
//! confirm exactly the plan shown and need a fresh elevation (M10 2);
//! pausing holds every write; held plans and failed writes show in "needs
//! you". A stateful Jellyfin fake from the recorded fixtures counts every
//! request other than `GET`.
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Value, json};
use starr_adapter_kit::AuthScheme;
use starr_adapter_kit::contract::media_server::{FakeFlavor, MediaServerFake, Recorded};

use super::adoption::{ALEX, KEY, decide, instance, movies};
use super::common::media::member;
use super::common::services::{Env, env};
use super::common::{get, post};

async fn fake() -> MediaServerFake {
    MediaServerFake::start(
        FakeFlavor::Jellyfin,
        AuthScheme::MediaBrowserToken,
        KEY,
        Recorded::from_dir(&format!(
            "{}/../adapters/jellyfin/fixtures/12.3.0/accounts",
            env!("CARGO_MANIFEST_DIR")
        )),
        "friend",
    )
    .await
}

async fn plan(env: &Env, id: i64) -> Value {
    let res = env
        .send(get(&format!("/api/v1/services/{id}/account-plan")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    res.body
}

fn confirm(id: i64, fingerprint: &Value) -> super::common::Req {
    post(
        &format!("/api/v1/services/{id}/account-plan/confirm"),
        json!({ "fingerprint": fingerprint }),
    )
}

async fn audit(env: &Env) -> Vec<String> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT action FROM audit_log WHERE action LIKE 'account%' ORDER BY id")
            .fetch_all(env.app.state.db.pool())
            .await
            .unwrap();
    rows.into_iter().map(|(a,)| a).collect()
}

/// The worker compares approvals with the wall clock: start the app's
/// manual clock there.
fn sync_clock(env: &Env) {
    env.app.clock.advance(Utc::now() - env.app.clock.now());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one go-live story
async fn going_live_pausing_and_resuming_confirm_exactly_the_plan_shown() {
    let mut env = env().await;
    sync_clock(&env);
    // Going live needs TOTP for every active admin (M10 1).
    env.enable_totp().await;
    env.elevate().await;
    let fake = fake().await;
    let alex = member(&env, "alex", movies()).await;
    let id = instance(&env, &fake.server.uri()).await;
    let res = env
        .send(decide(
            alex.id,
            id,
            json!({ "decision": "linked", "remoteId": ALEX }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    // Plan only after linking: the plan shows the change, nothing is sent.
    let shown = plan(&env, id).await;
    assert_eq!(shown["writes"]["mode"], "planOnly");
    assert_eq!(shown["writes"]["next"], "planOnly");
    assert_eq!(shown["writes"]["cap"], 5);
    assert_eq!(shown["counts"]["libraries"], 1);
    env.send(post("/api/v1/jobs/accounts.reconcile/trigger", json!({})))
        .await;
    env.drain().await;
    assert_eq!(fake.writes().await, vec![], "plan only sends nothing");
    let res = env
        .send(post(
            &format!("/api/v1/services/{id}/account-writes/pause"),
            json!({}),
        ))
        .await;
    assert_eq!(res.code(), "accounts.not_live");

    // Going live needs a fresh elevation…
    env.app.clock.advance(chrono::TimeDelta::minutes(5));
    let res = env.send(confirm(id, &shown["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    assert_eq!(res.code(), "auth.elevation_required");
    env.elevate().await;
    // …and exactly the plan shown.
    let res = env.send(confirm(id, &json!("not-the-plan"))).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "accounts.plan_changed");
    let shown = plan(&env, id).await;
    let res = env.send(confirm(id, &shown["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["writes"]["mode"], "live");
    assert_eq!(res.body["writes"]["liveBy"], "root");
    assert_eq!(res.body["writes"]["approved"], true);
    assert_eq!(res.body["writes"]["next"], "execute");
    env.drain().await;
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].target, format!("/Users/{ALEX}/Policy"));
    assert_eq!(writes[0].body["EnableAllFolders"], json!(false));
    let after = plan(&env, id).await;
    assert_eq!(after["counts"]["libraries"], 0);
    assert_eq!(after["writes"]["approved"], false);
    assert!(after["writes"]["lastLiveRunAt"].is_string());

    // Paused: a suspension is planned, not written.
    let res = env
        .send(post(
            &format!("/api/v1/services/{id}/account-writes/pause"),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["writes"]["mode"], "paused");
    assert_eq!(res.body["writes"]["pausedBy"], "root");
    let res = env
        .send(post(
            &format!("/api/v1/members/{}/suspend", alex.id),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.send(post("/api/v1/jobs/accounts.reconcile/trigger", json!({})))
        .await;
    env.drain().await;
    let paused = plan(&env, id).await;
    assert_eq!(paused["counts"]["disable"], 1);
    assert_eq!(paused["writes"]["next"], "paused");
    assert_eq!(fake.writes().await.len(), 1, "paused writes send nothing");

    // Resuming confirms the pending plan.
    let res = env.send(confirm(id, &paused["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    let writes = fake.writes().await;
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[1].body["IsDisabled"], json!(true));

    let actions = audit(&env).await;
    let tail: Vec<&str> = actions
        .iter()
        .map(String::as_str)
        .filter(|a| a.starts_with("accounts.") || *a != "account.link")
        .collect();
    assert_eq!(
        tail,
        [
            "accounts.live",
            "account.libraries",
            "accounts.pause",
            "accounts.resume",
            "account.disable"
        ]
    );
}

#[tokio::test]
async fn held_plans_and_failed_writes_need_the_admin() {
    let mut env = env().await;
    sync_clock(&env);
    // Going live needs TOTP for every active admin (M10 1).
    env.enable_totp().await;
    env.elevate().await;
    let fake = fake().await;
    let alex = member(&env, "alex", movies()).await;
    let id = instance(&env, &fake.server.uri()).await;
    env.send(decide(
        alex.id,
        id,
        json!({ "decision": "linked", "remoteId": ALEX }),
    ))
    .await;
    let shown = plan(&env, id).await;
    // A write that fails lands in "needs you".
    fake.fail_policy_writes();
    let res = env.send(confirm(id, &shown["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    let res = env.send(get("/api/v1/accounts/attention")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let failure = &res.body["failures"][0];
    assert_eq!(failure["username"], "alex");
    assert_eq!(failure["code"], "transient");
    assert_eq!(failure["serviceId"], id);

    // The service is back, but the last complete live run was two hours
    // ago (the approval was used then): the next write waits.
    fake.allow_policy_writes();
    let fingerprint = shown["fingerprint"].as_str().unwrap().to_owned();
    env.app
        .state
        .db
        .record_live_run(id, &fingerprint, Utc::now() - chrono::TimeDelta::hours(2))
        .await
        .unwrap();
    let res = env.send(get("/api/v1/accounts/attention")).await;
    let held = &res.body["held"][0];
    assert_eq!(held["serviceId"], id, "{}", res.raw);
    assert_eq!(held["reason"], "noRecentRun");
    let waiting = plan(&env, id).await;
    assert_eq!(waiting["writes"]["next"], "held");
    assert_eq!(waiting["writes"]["holdReason"], "noRecentRun");
    env.send(post("/api/v1/jobs/accounts.reconcile/trigger", json!({})))
        .await;
    env.drain().await;
    assert_eq!(fake.writes().await.len(), 1, "only the failed attempt");
    // Approving exactly this plan lets it run; the failure clears.
    let res = env.send(confirm(id, &waiting["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    assert_eq!(fake.writes().await.len(), 2);
    let res = env.send(get("/api/v1/accounts/attention")).await;
    assert_eq!(
        res.body,
        json!({
            "held": [],
            "failures": [],
            "withoutPassword": [],
            "conflicts": [],
            "deviations": [],
            "staleLibraries": [],
        })
    );
}

/// Spec 0008 ADM-06: a new admin without two-step verification pauses live
/// account writes by itself; the Inbox says why and offers to resume once
/// the reason is gone (here: the new admin removed again).
#[tokio::test]
async fn automatic_pauses_show_their_reason_and_offer_resume_once_cleared() {
    let mut env = env().await;
    sync_clock(&env);
    env.enable_totp().await;
    env.elevate().await;
    let fake = fake().await;
    let alex = member(&env, "alex", movies()).await;
    let id = instance(&env, &fake.server.uri()).await;
    env.send(decide(
        alex.id,
        id,
        json!({ "decision": "linked", "remoteId": ALEX }),
    ))
    .await;
    let shown = plan(&env, id).await;
    let res = env.send(confirm(id, &shown["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    let res = env
        .send(post("/api/v1/admins", json!({ "username": "lea" })))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let lea = res.body["admin"]["id"].as_i64().unwrap();
    let attention = env.send(get("/api/v1/admin/attention")).await;
    let paused = attention.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "writes_paused")
        .unwrap_or_else(|| panic!("{}", attention.raw))
        .clone();
    assert_eq!(paused["id"], format!("writes:{id}"));
    assert_eq!(paused["section"], "paused");
    assert_eq!(paused["writes"]["kind"], "accounts");
    assert_eq!(paused["writes"]["automatic"], true);
    assert_eq!(paused["writes"]["pausedBy"], Value::Null);
    assert_eq!(paused["writes"]["adminsWithoutTotp"], json!(["lea"]));
    assert_eq!(paused["writes"]["canResume"], false);

    // Lea removed again: the writes stay paused, but may be resumed now.
    let res = env
        .send(super::common::Req::new(
            axum::http::Method::DELETE,
            &format!("/api/v1/admins/{lea}"),
            Some(json!({ "username": "lea" })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    let attention = env.send(get("/api/v1/admin/attention")).await;
    let paused = attention.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "writes_paused")
        .unwrap_or_else(|| panic!("{}", attention.raw))
        .clone();
    assert_eq!(paused["writes"]["adminsWithoutTotp"], json!([]));
    assert_eq!(paused["writes"]["canResume"], true);
}

#[tokio::test]
async fn members_cannot_reach_account_writes() {
    let env = env().await;
    let fake = fake().await;
    let id = instance(&env, &fake.server.uri()).await;
    let mia = member(&env, "mia", movies()).await;
    for req in [
        confirm(id, &json!("x")),
        post(
            &format!("/api/v1/services/{id}/account-writes/pause"),
            json!({}),
        ),
        get("/api/v1/accounts/attention"),
    ] {
        let res = req.cookie(&mia.cookie).send(&env.app).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    }
    assert_eq!(fake.writes().await, vec![]);
}
