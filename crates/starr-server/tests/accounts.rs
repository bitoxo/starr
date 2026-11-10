//! Foreign account inventory (spec 0005 slice 1): admins see every account
//! of a consume instance with suggestions; members never see it; ignoring
//! is audited and writes nothing to the service; scans send only `GET`s.
#![allow(clippy::unwrap_used)]

mod common;
// Adoption and the account plan (spec 0005 slice 3), in this binary to keep
// CI's link count down.
#[path = "accounts/adoption.rs"]
mod adoption;
// Account writes with safe mode (spec 0005 slice 4).
#[path = "accounts/writes.rs"]
mod writes;
// Member passwords and My services (spec 0005 slice 5).
#[path = "accounts/passwords.rs"]
mod passwords;
// Lifecycle, deletion and hardening (spec 0005 slice 6).
#[path = "accounts/lifecycle.rs"]
mod lifecycle;

use axum::http::{Method, StatusCode};
use common::media::member;
use common::services::{Env, env};
use common::{Req, get, post};
use serde_json::{Value, json};
use wiremock::matchers::{any, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "jellyfin-accounts-key-0123456789";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/jellyfin/fixtures/12.3.0/accounts/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

/// A Jellyfin from recorded fixtures that answers `GET`s with the key and
/// records everything else.
async fn fake_jellyfin() -> MockServer {
    let server = MockServer::start().await;
    let auth = format!("MediaBrowser Token=\"{KEY}\"");
    for (route, file) in [
        ("/System/Info", "system_info.json"),
        ("/Users", "users.json"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("authorization", auth.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture(file)))
            .mount(&server)
            .await;
    }
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    server
}

async fn non_get_requests(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() != "GET")
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect()
}

/// A Jellyfin consume instance, scanned once.
async fn scanned(env: &Env, server: &MockServer) -> i64 {
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "jellyfin", "name": "Kino", "baseUrl": server.uri(), "apiKey": KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    let res = env
        .send(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "consume", "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let res = env
        .send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    env.drain().await;
    id
}

fn by_name<'a>(accounts: &'a Value, name: &str) -> &'a Value {
    accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["username"] == name)
        .unwrap_or_else(|| panic!("{name} missing"))
}

#[tokio::test]
async fn admins_see_every_account_with_suggestions() {
    let env = env().await;
    let server = fake_jellyfin().await;
    let alex = member(&env, "alex", json!([])).await;
    let friend = member(&env, "friend", json!([])).await;
    member(&env, "admin", json!([])).await;
    let id = scanned(&env, &server).await;

    let res = env
        .send(get(&format!("/api/v1/services/{id}/accounts")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["scanComplete"], true);
    assert!(res.body["scannedAt"].is_string());
    assert_eq!(res.body["scanError"], Value::Null);
    let accounts = &res.body["accounts"];
    assert_eq!(accounts.as_array().unwrap().len(), 7);

    // Equal names (ignoring case) are suggestions, nothing more.
    let a = by_name(accounts, "Alex");
    assert_eq!(a["suggestedMember"]["id"], alex.id);
    assert_eq!(a["suggestedMember"]["username"], "alex");
    assert_eq!(
        by_name(accounts, "friend")["suggestedMember"]["id"],
        friend.id
    );
    // Privileged accounts are marked and never offered, even with an equal
    // member name.
    let admin = by_name(accounts, "admin");
    assert_eq!(admin["privileged"], true);
    assert_eq!(admin["suggestedMember"], Value::Null);
    assert_eq!(
        by_name(accounts, "restricted")["suggestedMember"],
        Value::Null
    );
    let locked = by_name(accounts, "locked-friend");
    assert_eq!(
        (&locked["lockedByService"], &locked["enabled"]),
        (&json!(true), &json!(false))
    );
    assert_eq!(
        by_name(accounts, "restricted")["libraryIds"],
        json!(["f137a2dd21bbc1b99aa5c0f6bf02a805"])
    );
    assert_eq!(a["libraryIds"], Value::Null, "all libraries");
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn members_never_see_the_inventory() {
    let env = env().await;
    let server = fake_jellyfin().await;
    let alex = member(&env, "alex", json!([])).await;
    let mia = member(&env, "mia", json!([])).await;
    let id = scanned(&env, &server).await;
    let remote = "53bf201374d74f19b2dbc1986457da92";
    for cookie in [&alex.cookie, &mia.cookie] {
        let res = get(&format!("/api/v1/services/{id}/accounts"))
            .cookie(cookie)
            .send(&env.app)
            .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
        assert!(!res.raw.contains("Alex"), "{}", res.raw);
        let res = put(
            &format!("/api/v1/services/{id}/accounts/{remote}/ignored"),
            json!({ "ignored": true }),
        )
        .cookie(cookie)
        .send(&env.app)
        .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    }
    let res = get(&format!("/api/v1/services/{id}/accounts"))
        .send(&env.app)
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = env
        .send(get(&format!("/api/v1/services/{id}/accounts")))
        .await;
    let row = by_name(&res.body["accounts"], "Alex");
    assert_eq!(row["ignoredAt"], Value::Null, "untouched by the members");
}

#[tokio::test]
async fn ignoring_is_audited_and_writes_nothing_to_the_service() {
    let env = env().await;
    let server = fake_jellyfin().await;
    member(&env, "alex", json!([])).await;
    let id = scanned(&env, &server).await;
    let remote = "53bf201374d74f19b2dbc1986457da92";
    let url = format!("/api/v1/services/{id}/accounts/{remote}/ignored");

    let res = env.send(put(&url, json!({ "ignored": true }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["ignoredBy"], "root");
    assert!(res.body["ignoredAt"].is_string());
    assert_eq!(
        res.body["suggestedMember"],
        Value::Null,
        "ignored: no suggestion"
    );
    // Repeating it changes nothing and is not audited twice.
    let res = env.send(put(&url, json!({ "ignored": true }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);

    // A rescan keeps the flag.
    env.send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    env.drain().await;
    let res = env
        .send(get(&format!("/api/v1/services/{id}/accounts")))
        .await;
    assert_eq!(by_name(&res.body["accounts"], "Alex")["ignoredBy"], "root");

    let res = env.send(put(&url, json!({ "ignored": false }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["ignoredAt"], Value::Null);
    assert_eq!(res.body["suggestedMember"]["username"], "alex");

    let (actions,): (String,) = sqlx::query_as(
        "SELECT group_concat(action || ' ' || target, ',') FROM
           (SELECT action, target FROM audit_log WHERE action LIKE 'accounts.%' ORDER BY id)",
    )
    .fetch_one(env.app.state.db.pool())
    .await
    .unwrap();
    assert_eq!(
        actions,
        "accounts.ignore Kino:Alex,accounts.unignore Kino:Alex"
    );
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn unknown_accounts_and_services_without_accounts_are_refused() {
    let env = env().await;
    let server = fake_jellyfin().await;
    let id = scanned(&env, &server).await;
    let res = env
        .send(put(
            &format!("/api/v1/services/{id}/accounts/nope/ignored"),
            json!({ "ignored": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "accounts.not_found");
    let res = env.send(get("/api/v1/services/999/accounts")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "services.not_found");
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": "Radarr", "baseUrl": "http://radarr:7878", "apiKey": "k" }),
        ))
        .await;
    let radarr = res.body["service"]["id"].as_i64().unwrap();
    let res = env
        .send(get(&format!("/api/v1/services/{radarr}/accounts")))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "accounts.unsupported");
}
