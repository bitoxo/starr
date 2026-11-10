//! Account lifecycle and hardening (spec 0005 slice 6) through the API:
//! an active admin without TOTP pauses every live instance (M10 1,
//! maintainer decision 2026-11-03); deleting a member who owns service
//! accounts needs elevation (M10 2) and deletes them when the grace period
//! ends (D16); "needs you" lists conflicts and mapped libraries the service
//! no longer lists; a switch without the worker's key changes nothing.
use axum::http::{Method, StatusCode};
use chrono::Utc;
use serde_json::{Value, json};
use starr_adapter_kit::AuthScheme;
use starr_adapter_kit::contract::media_server::{FakeFlavor, MediaServerFake, Recorded, Write};

use super::adoption::{ALEX, KEY, decide, instance, movies};
use super::common::media::member;
use super::common::services::{Env, env};
use super::common::{Req, get, post};

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

fn confirm(id: i64, fingerprint: &Value) -> Req {
    post(
        &format!("/api/v1/services/{id}/account-plan/confirm"),
        json!({ "fingerprint": fingerprint }),
    )
}

/// A live Jellyfin consume instance with alex linked (admin with TOTP).
async fn live_with_alex(env: &mut Env, fake: &MediaServerFake) -> (i64, i64) {
    env.app.clock.advance(Utc::now() - env.app.clock.now());
    env.enable_totp().await;
    let alex = member(env, "alex", movies()).await;
    let id = instance(env, &fake.server.uri()).await;
    env.elevate().await;
    let res = env
        .send(decide(
            alex.id,
            id,
            json!({ "decision": "linked", "remoteId": ALEX }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let shown = plan(env, id).await;
    let res = env.send(confirm(id, &shown["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    assert_eq!(plan(env, id).await["writes"]["mode"], "live");
    (id, alex.id)
}

async fn audit_details(env: &Env, action: &str) -> Vec<Value> {
    let rows: Vec<(Option<String>,)> =
        sqlx::query_as("SELECT details FROM audit_log WHERE action = ? ORDER BY id")
            .bind(action)
            .fetch_all(env.app.state.db.pool())
            .await
            .unwrap();
    rows.into_iter()
        .map(|(d,)| serde_json::from_str(d.as_deref().unwrap_or("null")).unwrap())
        .collect()
}

#[tokio::test]
async fn turning_totp_off_while_live_pauses_writes_until_it_is_back() {
    let mut env = env().await;
    let fake = fake().await;
    let (id, alex) = live_with_alex(&mut env, &fake).await;
    let res = env.disable_totp().await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    let paused = plan(&env, id).await;
    assert_eq!(paused["writes"]["mode"], "paused");
    assert_eq!(paused["writes"]["pausedBy"], "starr");
    let audited = audit_details(&env, "accounts.pause").await;
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0]["reason"], "adminWithoutTotp");
    assert_eq!(audited[0]["automatic"], true);
    assert_eq!(audited[0]["admins"], json!(["root"]));
    assert_eq!(audited[0]["trigger"], "auth.totp_disabled");
    // Nothing is written while paused.
    let before = fake.writes().await.len();
    env.send(post(&format!("/api/v1/members/{alex}/suspend"), json!({})))
        .await;
    env.drain().await;
    assert_eq!(fake.writes().await.len(), before);
    // Resuming needs TOTP again.
    env.elevate().await;
    let shown = plan(&env, id).await;
    let res = env.send(confirm(id, &shown["fingerprint"])).await;
    assert_eq!(res.code(), "accounts.totp_required", "{}", res.raw);
    env.enable_totp().await;
    env.elevate().await;
    let res = env.send(confirm(id, &shown["fingerprint"])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    let writes = fake.writes().await;
    assert_eq!(writes.len(), before + 1);
    assert_eq!(writes[before].body["IsDisabled"], json!(true));
}

#[tokio::test]
async fn a_new_admin_without_totp_pauses_every_live_instance() {
    let mut env = env().await;
    let fake = fake().await;
    let (id, _) = live_with_alex(&mut env, &fake).await;
    let res = env
        .send(post("/api/v1/admins", json!({ "username": "second" })))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let paused = plan(&env, id).await;
    assert_eq!(paused["writes"]["mode"], "paused");
    assert_eq!(paused["adminsWithoutTotp"], json!(["second"]));
    let audited = audit_details(&env, "accounts.pause").await;
    assert_eq!(audited[0]["trigger"], "admin.created");
    assert_eq!(audited[0]["admins"], json!(["second"]));
}

#[tokio::test]
async fn deleting_a_member_with_accounts_needs_elevation_and_deletes_them_after_the_grace_period() {
    let mut env = env().await;
    let fake = fake().await;
    let (id, alex) = live_with_alex(&mut env, &fake).await;
    // As if Starr had created alex's account: only those are deleted (D16,
    // maintainer 2026-11-03; adopted ones are disabled and released).
    sqlx::query(
        "UPDATE service_account SET origin = 'provisioned', decision = 'create',
             password = 'starr', password_generation_written = 0
         WHERE member_id = ?",
    )
    .bind(alex)
    .execute(env.app.state.db.pool())
    .await
    .unwrap();
    let before = fake.writes().await.len();
    // Elevation expired: a deletion order that deletes accounts needs it.
    env.app.clock.advance(chrono::TimeDelta::minutes(6));
    let res = env
        .send(Req::new(
            Method::DELETE,
            &format!("/api/v1/members/{alex}"),
            Some(json!({ "username": "alex" })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    assert_eq!(res.code(), "auth.elevation_required");
    env.elevate().await;
    let res = env
        .send(Req::new(
            Method::DELETE,
            &format!("/api/v1/members/{alex}"),
            Some(json!({ "username": "alex" })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["outcome"], "scheduled");
    env.drain().await;
    // Disabled at once.
    let writes = fake.writes().await;
    assert_eq!(writes.len(), before + 1);
    assert_eq!(writes[before].body["IsDisabled"], json!(true));
    // The grace period ends: the account is deleted in the service.
    sqlx::query("UPDATE member_deletion SET grace_until = ? WHERE member_id = ?")
        .bind(
            (Utc::now() - chrono::TimeDelta::minutes(1))
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        )
        .bind(alex)
        .execute(env.app.state.db.pool())
        .await
        .unwrap();
    env.send(post("/api/v1/jobs/accounts.reconcile/trigger", json!({})))
        .await;
    env.drain().await;
    let writes = fake.writes().await;
    assert_eq!(
        writes[before + 1..].to_vec(),
        vec![Write {
            method: "DELETE".into(),
            target: format!("/Users/{ALEX}"),
            body: Value::Null,
        }]
    );
    let deletes = audit_details(&env, "account.delete").await;
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0]["result"], "ok");
    // Nothing left to plan for alex.
    let after = plan(&env, id).await;
    assert_eq!(after["counts"]["delete"], 0);
}

#[tokio::test]
async fn needs_you_lists_conflicts_and_libraries_the_service_no_longer_lists() {
    let mut env = env().await;
    let fake = fake().await;
    let (id, _) = live_with_alex(&mut env, &fake).await;
    // Alex's account gained admin rights in the service.
    fake.edit_user("Alex", |u| {
        u["Policy"]["IsAdministrator"] = json!(true);
    });
    // The movies library was renamed in Jellyfin (new ItemId).
    let role = env.app.state.db.list_roles(None).await.unwrap().remove(0);
    env.app
        .state
        .db
        .set_consume_libraries(
            role.id,
            &starr_core::libraries::ConsumeSettings {
                libraries: vec![starr_core::libraries::MappedLibrary {
                    id: "0000000000000000000000000000dead".into(),
                    grant_id: "0000000000000000000000000000dead".into(),
                }],
            },
            Utc::now(),
        )
        .await
        .unwrap();
    env.send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    env.drain().await;
    let res = env.send(get("/api/v1/accounts/attention")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        res.body["conflicts"],
        json!([{
            "serviceId": id,
            "serviceName": "Kino",
            "memberId": res.body["conflicts"][0]["memberId"],
            "username": "alex",
            "kind": "privileged",
            "since": null,
            "remoteId": null,
            "remoteName": null,
        }])
    );
    assert_eq!(
        res.body["staleLibraries"],
        json!([{
            "serviceId": id,
            "serviceName": "Kino",
            "category": "movies",
            "libraryId": "0000000000000000000000000000dead",
        }])
    );
    // The admins were told once.
    let res = env.send(get("/api/v1/admin/notifications")).await;
    let kinds: Vec<&str> = res.body["items"]
        .as_array()
        .map(|items| items.iter().filter_map(|n| n["kind"].as_str()).collect())
        .unwrap_or_default();
    assert!(kinds.contains(&"account_privileged"), "{}", res.raw);
}

#[tokio::test]
async fn a_switch_without_the_workers_key_is_refused_and_keeps_the_old_password() {
    let mut env = env().await;
    let fake = fake().await;
    let alex = member(&env, "alex", movies()).await;
    env.app.clock.advance(Utc::now() - env.app.clock.now());
    env.enable_totp().await;
    let id = instance(&env, &fake.server.uri()).await;
    env.elevate().await;
    env.send(decide(
        alex.id,
        id,
        json!({ "decision": "linked", "remoteId": ALEX }),
    ))
    .await;
    let shown = plan(&env, id).await;
    env.send(confirm(id, &shown["fingerprint"])).await;
    env.drain().await;
    // The worker has not published its key (e.g. split mode, worker down).
    sqlx::query("DELETE FROM app_meta WHERE key = 'service_credentials_recipient'")
        .execute(env.app.state.db.pool())
        .await
        .unwrap();
    let res = post(
        &format!("/api/v1/me/services/{id}/use-starr-password"),
        json!({ "password": "a long enough password" }),
    )
    .cookie(&alex.cookie)
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE, "{}", res.raw);
    assert_eq!(res.code(), "services.secrets_unavailable");
    let res = get("/api/v1/me/services")
        .cookie(&alex.cookie)
        .send(&env.app)
        .await;
    assert_eq!(res.body["services"][0]["state"], "oldPassword");
    let (handoffs,): (i64,) = sqlx::query_as("SELECT count(*) FROM password_handoff")
        .fetch_one(env.app.state.db.pool())
        .await
        .unwrap();
    assert_eq!(handoffs, 0);
}
