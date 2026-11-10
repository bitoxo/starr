//! Member passwords and My services (spec 0005 slice 5, ADR 0010) through
//! the API: going live needs TOTP for every active admin (M10 1); a new
//! member's accounts are created with the password set on accepting the
//! invitation; a password change reaches every account that uses the Starr
//! password; adopted accounts switch only when the member asks with the
//! current password (M2, M10 3). No response, audit entry or stored row
//! carries a password.
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Value, json};
use starr_adapter_kit::AuthScheme;
use starr_adapter_kit::contract::media_server::{FakeFlavor, MediaServerFake, Recorded};

use super::adoption::{ALEX, KEY, decide, instance, movies};
use super::common::media::member;
use super::common::services::{Env, env};
use super::common::{get, post};

const NEW_PASSWORD: &str = "Popcorn & \"Kino\" 🎬 nights";

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

/// Go live on `id` with the plan as shown (TOTP and elevation done).
async fn go_live(env: &mut Env, id: i64) {
    env.elevate().await;
    let shown = plan(env, id).await;
    let res = env
        .send(post(
            &format!("/api/v1/services/{id}/account-plan/confirm"),
            json!({ "fingerprint": shown["fingerprint"] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
}

/// A live Jellyfin consume instance for movies (admin with TOTP).
async fn live(env: &mut Env, fake: &MediaServerFake) -> i64 {
    env.app.clock.advance(Utc::now() - env.app.clock.now());
    env.enable_totp().await;
    let id = instance(env, &fake.server.uri()).await;
    go_live(env, id).await;
    id
}

async fn my_services(env: &Env, cookie: &str) -> Value {
    let res = get("/api/v1/me/services")
        .cookie(cookie)
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    res.body
}

/// Everything stored that could leak a password.
async fn stored_text(env: &Env) -> String {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT coalesce(details, '') FROM audit_log
         UNION ALL SELECT plan FROM account_plan
         UNION ALL SELECT coalesce(last_error_detail, '') FROM service_account
         UNION ALL SELECT payload FROM job
         UNION ALL SELECT coalesce(report, '') FROM run",
    )
    .fetch_all(env.app.state.db.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(t,)| t)
        .collect::<Vec<_>>()
        .join("\n")
}

async fn handoffs(env: &Env) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM password_handoff")
        .fetch_one(env.app.state.db.pool())
        .await
        .unwrap();
    n
}

#[tokio::test]
async fn going_live_resuming_and_approving_need_totp_for_every_admin() {
    let mut env = env().await;
    env.app.clock.advance(Utc::now() - env.app.clock.now());
    let fake = fake().await;
    let id = instance(&env, &fake.server.uri()).await;
    let shown = plan(&env, id).await;
    assert_eq!(shown["adminsWithoutTotp"], json!(["root"]));
    env.elevate().await;
    let res = env
        .send(post(
            &format!("/api/v1/services/{id}/account-plan/confirm"),
            json!({ "fingerprint": shown["fingerprint"] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "accounts.totp_required");
    assert_eq!(plan(&env, id).await["writes"]["mode"], "planOnly");
    // A second admin without TOTP blocks it as well.
    env.enable_totp().await;
    env.elevate().await;
    let res = env
        .send(post(
            "/api/v1/admins",
            json!({ "username": "ops", "displayName": "Ops" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let shown = plan(&env, id).await;
    assert_eq!(shown["adminsWithoutTotp"], json!(["ops"]));
    let res = env
        .send(post(
            &format!("/api/v1/services/{id}/account-plan/confirm"),
            json!({ "fingerprint": shown["fingerprint"] }),
        ))
        .await;
    assert_eq!(res.code(), "accounts.totp_required");
    assert_eq!(fake.writes().await, vec![]);
    let (refused,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM audit_log WHERE action = 'accounts.confirm_refused'")
            .fetch_one(env.app.state.db.pool())
            .await
            .unwrap();
    assert_eq!(refused, 2);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one member story
async fn a_new_member_logs_in_everywhere_with_the_password_set_on_accepting() {
    let mut env = env().await;
    let fake = fake().await;
    let id = live(&mut env, &fake).await;
    let res = env
        .send(post(
            "/api/v1/invitations",
            json!({ "username": "mia", "entitlements": movies() }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let token = res.body["link"]["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let res = post(
        &format!("/api/v1/invitations/{token}/accept"),
        json!({ "email": "mia@example.org", "password": "mia's first password" }),
    )
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let mia = res.session_cookie().unwrap();
    assert_eq!(handoffs(&env).await, 1, "sealed for the worker");
    // Until the worker ran, My services says it is being set up.
    let services = my_services(&env, &mia).await;
    assert_eq!(services["services"][0]["state"], "settingUp");
    env.drain().await;

    let writes = fake.writes().await;
    assert_eq!(writes[0].target, "/Users/New");
    assert_eq!(
        writes[0].body,
        json!({ "Name": "mia", "Password": "mia's first password" })
    );
    assert_eq!(handoffs(&env).await, 0, "deleted once written");
    let services = my_services(&env, &mia).await;
    let kino = &services["services"][0];
    assert_eq!(kino["serviceId"], id);
    assert_eq!(kino["name"], "Kino");
    assert_eq!(kino["state"], "ready");
    assert_eq!(kino["usesStarrPassword"], true);
    assert_eq!(kino["passwordChangeSignsOut"], true);
    assert_eq!(kino["username"], Value::Null);
    let remote = fake.user("mia").unwrap()["Id"].as_str().unwrap().to_owned();
    assert!(!services.to_string().contains(&remote), "no remote IDs");

    // A password change reaches the account (and signs out its devices).
    let res = post(
        "/api/v1/me/password",
        json!({ "currentPassword": "mia's first password", "newPassword": NEW_PASSWORD }),
    )
    .cookie(&mia)
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.raw);
    env.drain().await;
    let writes = fake.writes().await;
    let last = writes.last().unwrap();
    assert_eq!(last.target, format!("/Users/Password?userId={remote}"));
    assert_eq!(last.body, json!({ "NewPw": NEW_PASSWORD }));
    assert_eq!(
        my_services(&env, &mia).await["services"][0]["state"],
        "ready"
    );

    // Nothing is due any more: activating has nothing to do.
    let res = post(
        "/api/v1/me/services/activate",
        json!({ "password": NEW_PASSWORD }),
    )
    .cookie(&mia)
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "services.nothing_to_activate");

    let stored = stored_text(&env).await;
    assert!(!stored.contains("first password"), "no password stored");
    assert!(!stored.contains("Popcorn"), "no password stored");
    let actions: Vec<(String,)> =
        sqlx::query_as("SELECT action FROM audit_log WHERE action LIKE 'account.%' ORDER BY id")
            .fetch_all(env.app.state.db.pool())
            .await
            .unwrap();
    let actions: Vec<&str> = actions.iter().map(|(a,)| a.as_str()).collect();
    assert_eq!(actions, ["account.create", "account.password"]);
}

#[tokio::test]
async fn an_adopted_account_switches_only_when_the_member_asks_with_the_password() {
    let mut env = env().await;
    let fake = fake().await;
    let alex = member(&env, "alex", movies()).await;
    env.app.clock.advance(Utc::now() - env.app.clock.now());
    env.enable_totp().await;
    let id = instance(&env, &fake.server.uri()).await;
    env.elevate().await;
    let res = env
        .send(decide(
            alex.id,
            id,
            json!({ "decision": "linked", "remoteId": ALEX }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    go_live(&mut env, id).await;
    let before = fake.writes().await.len();
    let kino = &my_services(&env, &alex.cookie).await["services"][0];
    assert_eq!(kino["state"], "oldPassword");
    assert_eq!(kino["acceptsPassword"], true);

    // A wrong password counts against the login throttle and does nothing.
    let url = format!("/api/v1/me/services/{id}/use-starr-password");
    let res = post(&url, json!({ "password": "not my password" }))
        .cookie(&alex.cookie)
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "auth.current_password_invalid");
    assert_eq!(handoffs(&env).await, 0);

    let res = post(&url, json!({ "password": "a long enough password" }))
        .cookie(&alex.cookie)
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["services"][0]["state"], "settingUp");
    assert!(!res.raw.contains("long enough"));
    env.drain().await;
    let writes = fake.writes().await;
    assert_eq!(writes.len(), before + 1, "{writes:?}");
    assert_eq!(
        writes[before].target,
        format!("/Users/Password?userId={ALEX}")
    );
    assert_eq!(
        writes[before].body,
        json!({ "NewPw": "a long enough password" })
    );
    assert_eq!(
        my_services(&env, &alex.cookie).await["services"][0]["state"],
        "ready"
    );
    let res = post(&url, json!({ "password": "a long enough password" }))
        .cookie(&alex.cookie)
        .send(&env.app)
        .await;
    assert_eq!(res.code(), "services.already_starr_password");
    // Too many wrong passwords: throttled like a login (M10 3).
    let mut last = StatusCode::OK;
    for _ in 0..12 {
        let res = post(
            "/api/v1/me/services/activate",
            json!({ "password": "guess" }),
        )
        .cookie(&alex.cookie)
        .send(&env.app)
        .await;
        last = res.status;
        if last == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    // Admins have no My services.
    let res = env.send(get("/api/v1/me/services")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(!stored_text(&env).await.contains("long enough"));
}

#[tokio::test]
async fn nothing_switches_where_starr_does_not_write() {
    let mut env = env().await;
    let fake = fake().await;
    let alex = member(&env, "alex", movies()).await;
    let id = instance(&env, &fake.server.uri()).await;
    env.elevate().await;
    let res = env
        .send(decide(
            alex.id,
            id,
            json!({ "decision": "linked", "remoteId": ALEX }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let kino = &my_services(&env, &alex.cookie).await["services"][0];
    assert_eq!(kino["state"], "oldPassword");
    assert_eq!(kino["acceptsPassword"], false);
    let res = post(
        &format!("/api/v1/me/services/{id}/use-starr-password"),
        json!({ "password": "a long enough password" }),
    )
    .cookie(&alex.cookie)
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.raw);
    assert_eq!(res.code(), "services.nothing_to_activate");
    let res = env.send(get("/api/v1/accounts/matrix")).await;
    assert_eq!(res.body["members"][0]["cells"][0]["password"], "deferred");
    assert_eq!(handoffs(&env).await, 0);
    assert_eq!(fake.writes().await, vec![]);
}

/// My services says what it waits for (spec 0008 slice 6, MEM-22): a
/// service nobody decided on yet is listed (when the member has a category
/// there), a taken name and switched-off writes wait for the admin.
#[tokio::test]
async fn my_services_names_undecided_services_and_what_they_wait_for() {
    let mut env = env().await;
    let fake = fake().await;
    let alex = member(&env, "alex", movies()).await;
    let friend = member(&env, "friend", movies()).await;
    let mia = member(&env, "mia", movies()).await;
    let none = member(&env, "nina", json!([])).await;
    let id = instance(&env, &fake.server.uri()).await;

    let kino = &my_services(&env, &alex.cookie).await["services"][0];
    assert_eq!(kino["serviceId"], id);
    assert_eq!(kino["state"], "undecided");
    assert_eq!(kino["waitingFor"], Value::Null);
    // No category there: nothing to decide for this member.
    assert_eq!(my_services(&env, &none.cookie).await["services"], json!([]));

    env.elevate().await;
    for who in [friend.id, mia.id] {
        let res = env
            .send(decide(who, id, json!({ "decision": "create" })))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    }
    // "friend" is taken there: only an admin can sort that out.
    let kino = &my_services(&env, &friend.cookie).await["services"][0];
    assert_eq!(kino["state"], "waiting");
    assert_eq!(kino["waitingFor"], "nameTaken");
    // Plan only: nothing is written until the admin switches writes on.
    let kino = &my_services(&env, &mia.cookie).await["services"][0];
    assert_eq!(kino["state"], "waiting");
    assert_eq!(kino["waitingFor"], "writesOff");
    assert!(
        !kino.to_string().contains("08552f025edb4744"),
        "no remote IDs"
    );
}

#[tokio::test]
async fn invitations_naming_accounts_before_elevation_existed_do_not_adopt() {
    let mut env = env().await;
    let fake = fake().await;
    let id = instance(&env, &fake.server.uri()).await;
    env.elevate().await;
    let res = env
        .send(post(
            "/api/v1/invitations",
            json!({
                "username": "kim",
                "entitlements": movies(),
                "accounts": [{ "serviceId": id, "remoteId": ALEX }],
            }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let token = res.body["link"]["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    // As stored by slice 3, before naming accounts needed an elevation.
    sqlx::query("UPDATE invitation SET preset_accounts = json_array(json_object('serviceId', ?, 'remoteId', ?))")
        .bind(id)
        .bind(ALEX)
        .execute(env.app.state.db.pool())
        .await
        .unwrap();
    let res = post(
        &format!("/api/v1/invitations/{token}/accept"),
        json!({ "email": "kim@example.org", "password": "a long enough password" }),
    )
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    env.drain().await;
    let res = env.send(get("/api/v1/accounts/matrix")).await;
    let kim = res.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["username"] == "kim")
        .unwrap();
    assert_eq!(kim["cells"][0]["decision"], "undecided");
    let (details,): (String,) =
        sqlx::query_as("SELECT details FROM audit_log WHERE action = 'account.link_refused'")
            .fetch_one(env.app.state.db.pool())
            .await
            .unwrap();
    assert!(details.contains("Unconfirmed"), "{details}");
}
