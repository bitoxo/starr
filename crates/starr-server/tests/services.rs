//! Service instances end to end: admin API → sealed storage → `services.check`
//! job in the worker → probe against a recorded Radarr fake → status in the API.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use axum::http::StatusCode;
use common::services::{API_KEY, delete, env, fake_radarr, patch};
use common::{elevate_in_place, get, post, setup_admin, test_app};
use serde_json::{Value, json};

#[tokio::test]
async fn adding_radarr_checks_it_in_the_background_and_shows_the_result() {
    let env = env().await;
    let radarr = fake_radarr().await;

    let res = env
        .create("Movies", &format!("{}/", radarr.uri()), API_KEY)
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    assert!(!res.raw.contains(API_KEY), "API key echoed: {}", res.raw);
    let service = &res.body["service"];
    let id = service["id"].as_i64().unwrap();
    let job_id = res.body["jobId"].as_i64().unwrap();
    assert_eq!(service["baseUrl"], radarr.uri(), "trailing slash removed");
    assert_eq!(service["mode"], "probe");
    assert_eq!(service["lastCheckAt"], Value::Null);

    // Stored sealed, not in plaintext.
    let (sealed,): (Vec<u8>,) =
        sqlx::query_as("SELECT credential_sealed FROM service_instance WHERE id = ?")
            .bind(id)
            .fetch_one(env.app.state.db.pool())
            .await
            .unwrap();
    assert!(
        !sealed
            .windows(API_KEY.len())
            .any(|w| w == API_KEY.as_bytes())
    );

    env.drain().await;

    let res = env.send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["detectedVersion"], "6.4.4.10685");
    assert_eq!(res.body["capabilities"], json!(["requests"]));
    assert_eq!(res.body["lastError"], Value::Null);
    assert!(res.body["lastCheckAt"].is_string());

    // The run is findable by job id and carries a per-instance report.
    let runs = env.send(get(&format!("/api/v1/runs?jobId={job_id}"))).await;
    let run = &runs.body["runs"][0];
    assert_eq!(run["kind"], "services.check");
    assert_eq!(run["state"], "succeeded");
    assert_eq!(run["expectation"], "met");
    let detail = env.send(get(&format!("/api/v1/runs/{}", run["id"]))).await;
    let item = &detail.body["report"]["items"][0];
    assert_eq!(item["target"], "Movies (radarr)");
    assert_eq!(item["outcome"], "succeeded");

    let list = env.send(get("/api/v1/services")).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);

    let audit = env.app.state.db.recent_audit(10).await.unwrap();
    let created = audit
        .iter()
        .find(|e| e.action == "services.create")
        .unwrap();
    assert_eq!(created.actor_name.as_deref(), Some(common::ADMIN_USER));
    assert_eq!(created.target.as_deref(), Some("Movies"));
    assert!(!created.details.as_deref().unwrap().contains(API_KEY));
}

#[tokio::test]
async fn wrong_api_key_shows_credentials_invalid() {
    let env = env().await;
    let radarr = fake_radarr().await;
    let res = env.create("Movies", &radarr.uri(), "not-the-key").await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    let job_id = res.body["jobId"].as_i64().unwrap();

    env.drain().await;

    let res = env.send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(res.body["lastError"], "credentials_invalid");
    let runs = env.send(get(&format!("/api/v1/runs?jobId={job_id}"))).await;
    assert_eq!(runs.body["runs"][0]["expectation"], "unmet");
    let detail = env
        .send(get(&format!("/api/v1/runs/{}", runs.body["runs"][0]["id"])))
        .await;
    assert_eq!(
        detail.body["report"]["items"][0]["code"],
        "credentials_invalid"
    );

    // Fixing the key queues a new check and clears the old result.
    elevate_in_place(&env.app, &env.admin).await;
    let res = env
        .send(patch(
            &format!("/api/v1/services/{id}"),
            json!({ "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert!(res.body["jobId"].is_i64());
    assert_eq!(res.body["service"]["lastError"], Value::Null);
    assert_eq!(res.body["service"]["lastCheckAt"], Value::Null);
    env.drain().await;
    let res = env.send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(res.body["lastError"], Value::Null);
    assert_eq!(res.body["detectedVersion"], "6.4.4.10685");
}

/// After `secret.key` was lost, Starr restarts with a new key; the service
/// shows `credentials_unreadable` until the admin re-enters the API key.
#[tokio::test]
async fn re_entering_the_key_after_a_lost_secret_key_clears_the_state() {
    let env = env().await;
    let radarr = fake_radarr().await;
    let res = env.create("Movies", &radarr.uri(), API_KEY).await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.drain().await;

    // "Restart" with a missing key file: new key, new worker, new web state.
    let db = env.app.state.db.clone();
    let keys = common::services::KeyDir::new();
    let key = starr_worker::secrets::CredentialKey::init(&db, &keys.0.join("secret.key"))
        .await
        .unwrap();
    let worker = starr_worker::Worker::new(
        db.clone(),
        starr_worker::SystemJobs::new(key),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        starr_worker::WorkerConfig::default(),
    );
    // The web side keeps running (split mode): it must seal for the
    // recipient the restarted worker published, not a remembered one.
    let send = |req: common::Req| env.send(req);

    let res = send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(res.body["lastError"], "credentials_unreadable");
    elevate_in_place(&env.app, &env.admin).await;

    let res = send(patch(
        &format!("/api/v1/services/{id}"),
        json!({ "apiKey": API_KEY }),
    ))
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["service"]["lastError"], Value::Null);
    assert!(res.body["jobId"].is_i64());
    while worker
        .tick(&tokio_util::sync::CancellationToken::new())
        .await
        .unwrap()
    {}
    let res = send(get(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(res.body["lastError"], Value::Null);
    assert_eq!(res.body["detectedVersion"], "6.4.4.10685");
}

#[tokio::test]
async fn input_is_validated_with_codes() {
    let env = env().await;
    for (body, code) in [
        (
            json!({ "adapterType": "sabnzbd", "name": "x", "baseUrl": "http://s", "apiKey": "k" }),
            "validation.adapter_type",
        ),
        (
            json!({ "adapterType": "radarr", "name": " ", "baseUrl": "http://s", "apiKey": "k" }),
            "validation.service_name",
        ),
        (
            json!({ "adapterType": "radarr", "name": "x", "baseUrl": "radarr:7878", "apiKey": "k" }),
            "validation.base_url",
        ),
        (
            json!({ "adapterType": "radarr", "name": "x", "baseUrl": "http://u:p@s", "apiKey": "k" }),
            "validation.base_url",
        ),
        (
            json!({ "adapterType": "radarr", "name": "x", "baseUrl": "http://s", "apiKey": "a b" }),
            "validation.api_key",
        ),
    ] {
        let res = env.send(post("/api/v1/services", body.clone())).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(res.code(), code, "{body}");
    }
    assert_eq!(
        env.create("Movies", "http://radarr", "k").await.status,
        StatusCode::ACCEPTED
    );
    let res = env.create("movies", "http://other", "k").await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.code(), "services.name_taken");
}

#[tokio::test]
async fn rename_keeps_the_check_result_and_queues_nothing() {
    let env = env().await;
    let radarr = fake_radarr().await;
    let res = env.create("Movies", &radarr.uri(), API_KEY).await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.drain().await;

    let res = env
        .send(patch(
            &format!("/api/v1/services/{id}"),
            json!({ "name": "Films", "baseUrl": radarr.uri() }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["jobId"], Value::Null);
    assert_eq!(res.body["service"]["name"], "Films");
    assert_eq!(res.body["service"]["detectedVersion"], "6.4.4.10685");

    let res = env
        .send(patch("/api/v1/services/999", json!({ "name": "x" })))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.code(), "services.not_found");
    let audit = env.app.state.db.recent_audit(10).await.unwrap();
    let update = audit
        .iter()
        .find(|e| e.action == "services.update")
        .unwrap();
    assert_eq!(update.target.as_deref(), Some("Films"));
}

#[tokio::test]
async fn a_new_server_needs_the_key_again_and_an_elevation() {
    let mut env = env().await;
    let radarr = fake_radarr().await;
    let res = env.create("Movies", &radarr.uri(), API_KEY).await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    env.drain().await;
    let url = format!("/api/v1/services/{id}");
    let elsewhere = fake_radarr().await;

    // Changing the address or the key needs a fresh re-authentication.
    for body in [
        json!({ "baseUrl": elsewhere.uri(), "apiKey": API_KEY }),
        json!({ "apiKey": API_KEY }),
        json!({ "baseUrl": format!("{}/radarr", radarr.uri()) }),
    ] {
        let res = env.send(patch(&url, body.clone())).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{body}: {}", res.raw);
        assert_eq!(res.code(), "auth.elevation_required", "{body}");
    }
    env.elevate().await;

    // Another origin (here: another port) without the key: refused, so the
    // stored key never reaches a server it was not entered for.
    let res = env
        .send(patch(
            &url,
            json!({ "baseUrl": elsewhere.uri(), "confirm": true }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "validation.credential_required_for_new_origin");
    let service = env.send(get(&url)).await;
    assert_eq!(service.body["baseUrl"], radarr.uri());
    env.drain().await;
    assert!(elsewhere.received_requests().await.unwrap().is_empty());

    // With the key it moves.
    let res = env
        .send(patch(
            &url,
            json!({ "baseUrl": elsewhere.uri(), "apiKey": API_KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["service"]["baseUrl"], elsewhere.uri());
    env.drain().await;
    let service = env.send(get(&url)).await;
    assert_eq!(service.body["lastError"], Value::Null, "{}", service.raw);

    // A path on the same origin keeps the stored key.
    let same_origin = format!("{}/", elsewhere.uri());
    let res = env
        .send(patch(
            &url,
            json!({ "baseUrl": format!("{same_origin}radarr") }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        res.body["service"]["baseUrl"],
        format!("{same_origin}radarr")
    );
}

#[tokio::test]
async fn renaming_needs_no_elevation() {
    let env = env().await;
    let res = env.create("Movies", "http://radarr.invalid", API_KEY).await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    let res = env
        .send(patch(
            &format!("/api/v1/services/{id}"),
            json!({ "name": "Films", "baseUrl": "http://radarr.invalid/" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["jobId"], Value::Null);
}

#[tokio::test]
async fn delete_and_manual_check() {
    let env = env().await;
    let res = env.create("Movies", "http://radarr.invalid", API_KEY).await;
    let id = res.body["service"]["id"].as_i64().unwrap();
    let first_job = res.body["jobId"].as_i64().unwrap();

    // A pending check for the same instance is reused, not duplicated.
    let res = env
        .send(post(&format!("/api/v1/services/{id}/check"), json!({})))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(res.body["jobId"].as_i64(), Some(first_job));

    let res = env.send(delete(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    let res = env.send(delete(&format!("/api/v1/services/{id}"))).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = env
        .send(post(&format!("/api/v1/services/{id}/check"), json!({})))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // The queued check finds nothing and skips.
    env.drain().await;
    let runs = env
        .send(get(&format!("/api/v1/runs?jobId={first_job}")))
        .await;
    assert_eq!(runs.body["runs"][0]["state"], "succeeded");
    assert_eq!(runs.body["runs"][0]["expectation"], "not_applicable");

    let actions: Vec<String> = env
        .app
        .state
        .db
        .recent_audit(10)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert!(
        actions.contains(&"services.delete".to_owned()),
        "{actions:?}"
    );
    assert!(
        actions.contains(&"services.check".to_owned()),
        "{actions:?}"
    );
}

#[tokio::test]
async fn service_types_list_radarr_and_sonarr() {
    let env = env().await;
    let res = env.send(get("/api/v1/service-types")).await;
    assert_eq!(res.status, StatusCode::OK);
    let radarr = &res.body[0];
    assert_eq!(radarr["typeId"], "radarr");
    assert_eq!(radarr["displayName"], "Radarr");
    assert_eq!(radarr["capabilities"], json!(["requests"]));
    assert_eq!(radarr["categories"], json!(["movies"]));
    assert_eq!(radarr["testedVersions"]["min"], "5.0.0.0");
    let sonarr = &res.body[1];
    assert_eq!(sonarr["typeId"], "sonarr");
    assert_eq!(sonarr["displayName"], "Sonarr");
    assert_eq!(sonarr["capabilities"], json!(["requests"]));
    assert_eq!(sonarr["testedVersions"]["min"], "4.0.4.0");
    assert_eq!(sonarr["testedVersions"]["maxExclusive"], "5.0.0.0");
    let types: Vec<_> = res.body.as_array().unwrap().iter().collect();
    for consume in ["jellyfin", "emby"] {
        let ty = types.iter().find(|t| t["typeId"] == consume).unwrap();
        assert_eq!(
            ty["capabilities"],
            json!(["libraries", "accounts"]),
            "{consume}"
        );
    }
}

#[tokio::test]
async fn consume_services_keep_a_separate_public_url_for_links() {
    let env = env().await;
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "jellyfin", "name": "Jellyfin", "baseUrl": "http://jellyfin:8096",
                    "apiKey": "k", "publicUrl": "https://watch.example.com/" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    assert_eq!(
        res.body["service"]["publicUrl"],
        "https://watch.example.com"
    );
    env.drain().await;
    let checked = env.send(get(&format!("/api/v1/services/{id}"))).await;

    // Changing only the public URL queues no check and keeps its result.
    let uri = format!("/api/v1/services/{id}");
    let res = env
        .send(patch(
            &uri,
            json!({ "publicUrl": "https://jf.example.com" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["jobId"], Value::Null);
    assert_eq!(res.body["service"]["publicUrl"], "https://jf.example.com");
    assert_eq!(
        res.body["service"]["lastCheckAt"],
        checked.body["lastCheckAt"]
    );
    // Omitted keeps it; null clears it.
    let res = env.send(patch(&uri, json!({ "name": "JF" }))).await;
    assert_eq!(res.body["service"]["publicUrl"], "https://jf.example.com");
    let res = env.send(patch(&uri, json!({ "publicUrl": null }))).await;
    assert_eq!(res.body["service"]["publicUrl"], Value::Null);
    // "" clears as well.
    env.send(patch(
        &uri,
        json!({ "publicUrl": "https://jf.example.com" }),
    ))
    .await;
    let res = env.send(patch(&uri, json!({ "publicUrl": "" }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["service"]["publicUrl"], Value::Null);

    let res = env
        .send(patch(&uri, json!({ "publicUrl": "jf.example.com" })))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "validation.public_url");
    // Services without libraries build no links.
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": "Movies", "baseUrl": "http://radarr",
                    "apiKey": "k", "publicUrl": "https://radarr.example.com" }),
        ))
        .await;
    assert_eq!(res.code(), "validation.public_url");
}

#[tokio::test]
async fn without_a_published_key_creating_fails_with_503() {
    // Split mode before the worker ran once: no recipient in app_meta.
    let app = test_app().await;
    let admin = setup_admin(&app).await;
    let res = post(
        "/api/v1/services",
        json!({ "adapterType": "radarr", "name": "x", "baseUrl": "http://r", "apiKey": "k" }),
    )
    .cookie(&admin)
    .send(&app)
    .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.code(), "services.secrets_unavailable");
}

/// ADR 0003: the web server code never touches the key that opens service
/// credentials. `CredentialKey::open` is crate-private to `starr-worker`
/// (module visibility); this test keeps the server crate from reaching for
/// the raw key type around it. Only `main.rs` may hand a `CredentialKey`
/// to the worker. Amendment 1: the web side may open catalog tokens, and
/// only through `CatalogTokenKey`/`CatalogAccess`, which open the
/// `CatalogTokens` purpose alone (a service credential is refused by class
/// and by recipient; see `catalog_access_opens_catalog_tokens_only` in
/// starr-worker and `catalog_tokens_are_sealed_with_the_web_key_*`).
#[test]
fn web_code_has_no_access_to_the_credential_key() {
    // The server gets `CredentialSealer` (re-exported by starr-worker), its
    // own `TotpKey` and `CatalogTokenKey` from starr-secrets; it must never
    // name the raw `SecretKey` (which could load the worker's key file),
    // the credentials purpose, or a generic opener. Checked on the source
    // text below.
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut dirs = vec![src.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    assert!(files.len() > 10, "found only {files:?}");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let rel = file.strip_prefix(&src).unwrap();
        for forbidden in [
            "SecretKey<",
            "SecretKey::",
            "ServiceCredentials",
            "open_stored",
            "reseal_catalog_tokens",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} mentions `{forbidden}`: only the worker may open service credentials",
                rel.display()
            );
        }
        if rel != std::path::Path::new("main.rs") {
            assert!(
                !text.contains("CredentialKey"),
                "{} mentions `CredentialKey`; only main.rs wires it into the worker",
                rel.display()
            );
        }
    }
}
