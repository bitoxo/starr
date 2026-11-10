//! Service accounts, adoption and the account plan (spec 0005 slice 3):
//! admins decide per member and instance (link / create / none), equal
//! names are only suggestions, invitations can name existing accounts, and
//! the plan shows what Starr would do. Plan only: no request but `GET` ever
//! reaches the service, and planning itself sends none at all.
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{any, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::common::media::member;
use super::common::services::{Env, env};
use super::common::{Req, get, post};

pub(super) const KEY: &str = "jellyfin-adoption-key-0123456789";
pub(super) const ALEX: &str = "53bf201374d74f19b2dbc1986457da92";
const FRIEND: &str = "08552f025edb4744beacbbbb80ea75fe";
const RESTRICTED: &str = "237f7023497e4c7893306d3a7ece5044";
const ADMIN: &str = "951b2f041bac4fe0a0c762f02f5117b7";
const MOVIES: &str = "f137a2dd21bbc1b99aa5c0f6bf02a805";

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

/// A Jellyfin from recorded fixtures (users and libraries) that answers
/// `GET`s with the key and records everything else.
async fn fake_jellyfin() -> MockServer {
    let server = MockServer::start().await;
    let auth = format!("MediaBrowser Token=\"{KEY}\"");
    for (route, file) in [
        ("/System/Info", "system_info.json"),
        ("/Users", "users.json"),
        ("/Library/VirtualFolders", "virtual_folders.json"),
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

pub(super) fn movies() -> Value {
    json!([{ "category": "movies", "enabled": true, "requestMode": "approval" }])
}

/// The Jellyfin as consume instance for movies (switched on, "Movies"
/// mapped), scanned and planned once.
pub(super) async fn instance(env: &Env, base_url: &str) -> i64 {
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "jellyfin", "name": "Kino", "baseUrl": base_url, "apiKey": KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    let res = env
        .send(Req::new(
            Method::PATCH,
            "/api/v1/categories/movies",
            Some(json!({ "enabled": true })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = env
        .send(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "consume", "serviceId": id }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    let role = res.body["consumers"][0]["id"].clone();
    env.send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    env.drain().await;
    let res = env
        .send(put(
            &format!("/api/v1/categories/movies/roles/{role}/libraries"),
            json!({ "libraryIds": [MOVIES] }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    id
}

fn row<'a>(matrix: &'a Value, username: &str) -> &'a Value {
    matrix["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["username"] == username)
        .unwrap_or_else(|| panic!("{username} missing"))
}

fn entry<'a>(plan: &'a Value, username: &str) -> &'a Value {
    plan["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["member"]["username"] == username)
        .unwrap_or_else(|| panic!("{username} missing"))
}

pub(super) fn decide(member: i64, service: i64, body: Value) -> Req {
    put(
        &format!("/api/v1/members/{member}/service-accounts/{service}"),
        body,
    )
}

#[tokio::test]
async fn existing_members_are_undecided_and_get_suggestions_but_no_steps() {
    let mut env = env().await;
    env.elevate().await;
    let server = fake_jellyfin().await;
    member(&env, "alex", movies()).await;
    member(&env, "sam", movies()).await;
    member(&env, "admin", movies()).await;
    let id = instance(&env, &server.uri()).await;

    let res = env.send(get("/api/v1/accounts/matrix")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["services"][0]["id"], id);
    assert_eq!(res.body["services"][0]["scanComplete"], true);
    assert!(res.body["services"][0]["plannedAt"].is_string());
    let alex = &row(&res.body, "alex")["cells"][0];
    assert_eq!(alex["decision"], "undecided");
    assert_eq!(alex["state"], "undecided");
    assert_eq!(alex["suggestion"]["remoteId"], ALEX);
    assert_eq!(alex["suggestion"]["username"], "Alex");
    assert_eq!(alex["steps"], json!([]));
    // A privileged account is never suggested, even with an equal name.
    assert_eq!(
        row(&res.body, "admin")["cells"][0]["suggestion"],
        Value::Null
    );
    assert_eq!(row(&res.body, "sam")["cells"][0]["suggestion"], Value::Null);

    let res = env
        .send(get(&format!("/api/v1/services/{id}/account-plan")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    // Every instance starts plan only.
    assert_eq!(res.body["writes"]["mode"], "planOnly");
    assert_eq!(res.body["writes"]["next"], "planOnly");
    assert_eq!(res.body["complete"], true);
    assert_eq!(res.body["counts"]["undecided"], 3);
    for e in res.body["entries"].as_array().unwrap() {
        assert_eq!(e["steps"], json!([]), "undecided members get no step");
    }
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one adoption story
async fn linking_adopts_an_account_and_the_plan_shows_its_changes() {
    let mut env = env().await;
    env.elevate().await;
    let server = fake_jellyfin().await;
    let alex = member(&env, "alex", movies()).await;
    let sam = member(&env, "sam", movies()).await;
    let id = instance(&env, &server.uri()).await;
    let requests_before = server.received_requests().await.unwrap().len();

    // Adopting needs a fresh re-authentication (M10 2).
    let stale = env.admin.clone();
    env.app.clock.advance(chrono::TimeDelta::minutes(5));
    let res = env
        .send(decide(
            sam.id,
            id,
            json!({ "decision": "linked", "remoteId": FRIEND }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    assert_eq!(res.code(), "auth.elevation_required");
    let res = env
        .send(post(
            &format!(
                "/api/v1/members/{}/service-accounts/confirm-suggestions",
                alex.id
            ),
            json!({}),
        ))
        .await;
    assert_eq!(res.code(), "auth.elevation_required");
    // Deciding "create" or "none" needs none.
    let res = env
        .send(decide(sam.id, id, json!({ "decision": "none" })))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.elevate().await;
    assert_ne!(env.admin, stale);

    // Sam is linked by hand to an account with another name.
    let res = env
        .send(decide(
            sam.id,
            id,
            json!({ "decision": "linked", "remoteId": FRIEND }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["decision"], "linked");
    assert_eq!(res.body["origin"], "adopted");
    assert_eq!(res.body["password"], "deferred");
    assert_eq!(res.body["linkedUsername"], "friend");
    assert_eq!(res.body["account"]["remoteId"], FRIEND);
    assert_eq!(res.body["decidedBy"], "root");
    assert_eq!(res.body["state"], "managed");
    // "friend" sees every library: Starr would restrict it to Movies.
    let step = &res.body["steps"][0];
    assert_eq!(step["action"], "setLibraries");
    assert_eq!(step["reason"], "librariesDiffer");
    assert_eq!(step["fromAll"], true);
    assert_eq!(step["destructive"], true);
    assert_eq!(step["add"], json!([{ "id": MOVIES, "name": "Movies" }]));

    // Alex: one click confirms the equal-name suggestion.
    let res = env
        .send(post(
            &format!(
                "/api/v1/members/{}/service-accounts/confirm-suggestions",
                alex.id
            ),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let cell = &res.body["cells"][0];
    assert_eq!(cell["decision"], "linked");
    assert_eq!(cell["account"]["remoteId"], ALEX);
    assert_eq!(cell["suggestion"], Value::Null);

    // The inventory shows who owns what and suggests nothing for it.
    let res = env
        .send(get(&format!("/api/v1/services/{id}/accounts")))
        .await;
    let accounts = res.body["accounts"].as_array().unwrap();
    let alex_row = accounts.iter().find(|a| a["remoteId"] == ALEX).unwrap();
    assert_eq!(alex_row["linkedMember"]["username"], "alex");
    assert_eq!(alex_row["suggestedMember"], Value::Null);
    // A linked account cannot be ignored.
    let res = env
        .send(put(
            &format!("/api/v1/services/{id}/accounts/{ALEX}/ignored"),
            json!({ "ignored": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "accounts.linked");

    let res = env
        .send(get(&format!("/api/v1/members/{}/account-plan", sam.id)))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body[0]["serviceName"], "Kino");
    assert_eq!(res.body[0]["cell"]["steps"][0]["action"], "setLibraries");

    // Suspending a member is planned as a disable (queued replan).
    let res = env
        .send(post(
            &format!("/api/v1/members/{}/suspend", sam.id),
            json!({}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    env.drain().await;
    let res = env
        .send(get(&format!("/api/v1/services/{id}/account-plan")))
        .await;
    let sam_entry = entry(&res.body, "sam");
    assert_eq!(sam_entry["steps"][0]["action"], "disable");
    assert_eq!(sam_entry["steps"][0]["reason"], "suspended");
    assert_eq!(res.body["counts"]["disable"], 1);

    // Unlinking releases the account again; audited both ways.
    let res = env
        .send(decide(sam.id, id, json!({ "decision": "none" })))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["account"], Value::Null);
    assert_eq!(res.body["state"], "noAccount");
    let (actions,): (String,) = sqlx::query_as(
        "SELECT group_concat(action || ' ' || target, ',') FROM
           (SELECT action, target FROM audit_log WHERE action LIKE 'account.%' ORDER BY id)",
    )
    .fetch_one(env.app.state.db.pool())
    .await
    .unwrap();
    assert_eq!(
        actions,
        "account.decide Kino:sam,account.link Kino:sam,account.link Kino:alex,account.unlink Kino:sam,account.decide Kino:sam"
    );

    // Deciding and planning sent nothing to the service.
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        requests_before
    );
}

#[tokio::test]
async fn links_are_refused_for_privileged_ignored_unknown_and_owned_accounts() {
    let mut env = env().await;
    env.elevate().await;
    let server = fake_jellyfin().await;
    let alex = member(&env, "alex", movies()).await;
    let sam = member(&env, "sam", movies()).await;
    let id = instance(&env, &server.uri()).await;
    env.send(put(
        &format!("/api/v1/services/{id}/accounts/{RESTRICTED}/ignored"),
        json!({ "ignored": true }),
    ))
    .await;
    let link = |remote: &str| json!({ "decision": "linked", "remoteId": remote });
    for (body, status, code) in [
        (
            link(ADMIN),
            StatusCode::UNPROCESSABLE_ENTITY,
            "accounts.privileged",
        ),
        (
            link(RESTRICTED),
            StatusCode::UNPROCESSABLE_ENTITY,
            "accounts.ignored",
        ),
        (link("nope"), StatusCode::NOT_FOUND, "accounts.not_found"),
        (
            json!({ "decision": "linked" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "accounts.remote_id_required",
        ),
    ] {
        let res = env.send(decide(alex.id, id, body)).await;
        assert_eq!((res.status, res.code()), (status, code), "{}", res.raw);
    }
    let res = env.send(decide(alex.id, id, link(ALEX))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = env.send(decide(sam.id, id, link(ALEX))).await;
    assert_eq!(
        (res.status, res.code()),
        (StatusCode::CONFLICT, "accounts.taken")
    );
    let res = env.send(decide(999, id, link(ALEX))).await;
    assert_eq!(res.code(), "member.not_found");
    let res = env.send(decide(sam.id, 999, link(ALEX))).await;
    assert_eq!(res.code(), "services.not_found");

    // Members see none of it.
    for req in [
        get("/api/v1/accounts/matrix"),
        get(&format!("/api/v1/services/{id}/account-plan")),
        get(&format!("/api/v1/members/{}/account-plan", sam.id)),
        decide(sam.id, id, json!({ "decision": "create" })),
    ] {
        let res = req.cookie(&sam.cookie).send(&env.app).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
    }
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn new_members_are_provisioned_and_names_taken_are_conflicts() {
    let mut env = env().await;
    env.elevate().await;
    let server = fake_jellyfin().await;
    instance(&env, &server.uri()).await;
    // Created after the instance got accounts: decided "create" by Starr.
    let sam = member(&env, "sam", movies()).await;
    let restricted = member(&env, "restricted", movies()).await;
    let mia = member(&env, "mia", json!([])).await;
    env.drain().await;

    let res = env
        .send(get(&format!("/api/v1/members/{}/account-plan", sam.id)))
        .await;
    let cell = &res.body[0]["cell"];
    assert_eq!(cell["decision"], "create");
    assert_eq!(cell["decidedBy"], Value::Null);
    assert_eq!(cell["state"], "toCreate");
    assert_eq!(cell["steps"][0]["action"], "create");
    assert_eq!(cell["steps"][0]["username"], "sam");
    assert_eq!(cell["steps"][0]["add"][0]["name"], "Movies");

    let res = env
        .send(get(&format!(
            "/api/v1/members/{}/account-plan",
            restricted.id
        )))
        .await;
    let cell = &res.body[0]["cell"];
    assert_eq!(cell["state"], "nameTaken");
    assert_eq!(cell["conflictAccount"]["remoteId"], RESTRICTED);
    assert_eq!(cell["steps"], json!([]));
    // The Inbox resolves it in the row (spec 0008 ADM-08): it names the
    // account to link instead.
    let res = env.send(get("/api/v1/admin/attention")).await;
    let conflict = res.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "account_conflict")
        .unwrap_or_else(|| panic!("{}", res.raw))
        .clone();
    assert_eq!(conflict["section"], "fix");
    assert_eq!(conflict["conflict"]["kind"], "nameTaken");
    assert_eq!(conflict["conflict"]["memberId"], restricted.id);
    assert_eq!(conflict["conflict"]["remoteId"], RESTRICTED);
    assert_eq!(conflict["conflict"]["remoteName"], "restricted");

    let res = env
        .send(get(&format!("/api/v1/members/{}/account-plan", mia.id)))
        .await;
    assert_eq!(res.body[0]["cell"]["state"], "notNeeded");
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn invitations_name_existing_accounts_that_accepting_links() {
    let mut env = env().await;
    env.elevate().await;
    let server = fake_jellyfin().await;
    let id = instance(&env, &server.uri()).await;
    let accounts = |remote: &str| json!([{ "serviceId": id, "remoteId": remote }]);
    let alex = member(&env, "alex", movies()).await;
    env.send(decide(
        alex.id,
        id,
        json!({ "decision": "linked", "remoteId": ALEX }),
    ))
    .await;
    env.send(put(
        &format!("/api/v1/services/{id}/accounts/{RESTRICTED}/ignored"),
        json!({ "ignored": true }),
    ))
    .await;
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": "Radarr", "baseUrl": "http://radarr:7878", "apiKey": "k" }),
        ))
        .await;
    let radarr = res.body["service"]["id"].as_i64().unwrap();

    for (body, code) in [
        (accounts(ADMIN), "accounts.privileged"),
        (accounts("nope"), "accounts.not_found"),
        (accounts(ALEX), "accounts.taken"),
        (accounts(RESTRICTED), "accounts.ignored"),
        (
            json!([{ "serviceId": id, "remoteId": FRIEND }, { "serviceId": id, "remoteId": ALEX }]),
            "invitations.accounts_duplicate_service",
        ),
        (
            json!([{ "serviceId": 999, "remoteId": FRIEND }]),
            "services.not_found",
        ),
        (
            json!([{ "serviceId": radarr, "remoteId": FRIEND }]),
            "accounts.unsupported",
        ),
    ] {
        let res = env
            .send(post(
                "/api/v1/invitations",
                json!({ "accounts": body, "entitlements": movies() }),
            ))
            .await;
        assert_eq!(res.code(), code, "{}", res.raw);
    }
    // Without a category the server serves, the account would be disabled
    // right after accepting: refused.
    let res = env
        .send(post(
            "/api/v1/invitations",
            json!({ "accounts": accounts(FRIEND) }),
        ))
        .await;
    assert_eq!(
        res.code(),
        "invitations.accounts_not_entitled",
        "{}",
        res.raw
    );

    let res = env
        .send(post(
            "/api/v1/invitations",
            json!({ "username": "kim", "entitlements": movies(), "accounts": accounts(FRIEND) }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    assert_eq!(
        res.body["invitation"]["presetAccounts"],
        json!([{ "serviceId": id, "remoteId": FRIEND, "username": "friend" }])
    );
    let token = res.body["link"]["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    // The invited person never sees remote IDs.
    let res = get(&format!("/api/v1/invitations/{token}"))
        .send(&env.app)
        .await;
    assert!(!res.raw.contains(FRIEND), "{}", res.raw);
    let res = post(
        &format!("/api/v1/invitations/{token}/accept"),
        json!({ "email": "kim@example.org", "password": "a long enough password" }),
    )
    .send(&env.app)
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
    env.drain().await;

    let res = env.send(get("/api/v1/accounts/matrix")).await;
    let cell = &row(&res.body, "kim")["cells"][0];
    assert_eq!(cell["decision"], "linked");
    assert_eq!(cell["account"]["remoteId"], FRIEND);
    assert_eq!(cell["password"], "deferred");
    assert_eq!(cell["decidedBy"], Value::Null);
    assert_eq!(cell["state"], "managed");
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}
