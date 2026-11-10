#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::{TimeDelta, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use starr_core::jobs::{Expectation, JobKind, Report, Trigger};
use starr_db::Database;
use starr_db::auth::NewAdmin;
use starr_db::jobs::NewJob;
use starr_db::runs::RunEnd;
use starr_server::{AppState, HttpSettings, app};
use tower::ServiceExt;
use url::Url;

async fn call(db: &Database, method: &str, path: &str) -> (StatusCode, Value) {
    let settings = HttpSettings {
        public_url: Some(Url::parse(PUBLIC_URL).unwrap()),
        ..HttpSettings::default()
    };
    let router = app(AppState::new(db.clone(), settings));
    let cookie = admin_cookie(db, &router).await;
    let res = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::ORIGIN, PUBLIC_URL)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

const PUBLIC_URL: &str = "https://starr.test";

/// Create an admin on first use, then log in; returns the session cookie.
async fn admin_cookie(db: &Database, router: &axum::Router) -> String {
    if !db.admin_exists().await.unwrap() {
        let hash = starr_server::auth::crypto::hash_password(&"admin password".into()).unwrap();
        db.create_first_admin(
            &NewAdmin {
                username: "root",
                display_name: "Root",
                password_hash: &hash,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    }
    let mut req = Request::post("/api/v1/auth/login")
        .header(header::ORIGIN, PUBLIC_URL)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "username": "root", "password": "admin password" }).to_string(),
        ))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    res.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("starr_session="))
        .and_then(|c| c.split(';').next())
        .unwrap()
        .to_owned()
}

/// Create a finished run of `kind`; `ok` decides succeeded vs failed.
async fn finished_run(db: &Database, kind: &str, ok: bool) -> i64 {
    let now = Utc::now();
    db.enqueue_job(
        &NewJob {
            kind,
            payload: "{}",
            unique_key: None,
            trigger: Trigger::Manual,
            run_after: now - TimeDelta::seconds(1),
            max_attempts: 1,
        },
        now,
    )
    .await
    .unwrap();
    let job = db
        .claim_job("test", &[kind], now, Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    let run = db.start_run(&job, now).await.unwrap();
    let report = Report::new(1, 1, 0, 0, Expectation::Met("done".into()))
        .unwrap()
        .with_counter("things", 1);
    let end = if ok {
        RunEnd::Succeeded(&report)
    } else {
        RunEnd::Failed("boom")
    };
    db.finish_run(run, end, now).await.unwrap();
    db.complete_job(job.id, "test").await.unwrap();
    run
}

fn ids(page: &Value) -> Vec<i64> {
    page["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn runs_list_is_empty_initially() {
    let db = Database::open_in_memory().await.unwrap();
    let (status, body) = call(&db, "GET", "/api/v1/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["runs"], Value::Array(vec![]));
    assert_eq!(body["nextBefore"], Value::Null);
}

#[tokio::test]
async fn runs_list_is_newest_first_filtered_and_paginated() {
    let db = Database::open_in_memory().await.unwrap();
    let a1 = finished_run(&db, "system.noop", true).await;
    let b = finished_run(&db, "system.housekeeping", false).await;
    let a2 = finished_run(&db, "system.noop", true).await;

    let (status, body) = call(&db, "GET", "/api/v1/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&body), vec![a2, b, a1]);
    let first = &body["runs"][0];
    assert_eq!(first["kind"], "system.noop");
    assert_eq!(first["state"], "succeeded");
    assert_eq!(first["trigger"], "manual");
    assert_eq!(first["expectation"], "met");
    assert!(first["startedAt"].as_str().unwrap().ends_with('Z'));

    let (_, page1) = call(&db, "GET", "/api/v1/runs?limit=2").await;
    assert_eq!(ids(&page1), vec![a2, b]);
    let next = page1["nextBefore"].as_i64().unwrap();
    let (_, page2) = call(&db, "GET", &format!("/api/v1/runs?limit=2&before={next}")).await;
    assert_eq!(ids(&page2), vec![a1]);
    assert_eq!(page2["nextBefore"], Value::Null);

    let (_, by_kind) = call(&db, "GET", "/api/v1/runs?kind=system.noop").await;
    assert_eq!(ids(&by_kind), vec![a2, a1]);
    let (_, failed) = call(&db, "GET", "/api/v1/runs?state=failed").await;
    assert_eq!(ids(&failed), vec![b]);
    assert_eq!(failed["runs"][0]["error"], "boom");

    // Outcome combines state and expectation (ADM-29), with the job filter.
    let (_, ok) = call(&db, "GET", "/api/v1/runs?outcome=succeeded").await;
    assert_eq!(ids(&ok), vec![a2, a1]);
    let (_, bad) = call(&db, "GET", "/api/v1/runs?outcome=failed&kind=system.noop").await;
    assert_eq!(ids(&bad), Vec::<i64>::new());
    let (_, attention) = call(&db, "GET", "/api/v1/runs?outcome=attention").await;
    assert_eq!(ids(&attention), Vec::<i64>::new());
    let (status, _) = call(&db, "GET", "/api/v1/runs?outcome=exploded").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn runs_list_rejects_unknown_state() {
    let db = Database::open_in_memory().await.unwrap();
    let (status, _) = call(&db, "GET", "/api/v1/runs?state=exploded").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn run_detail_has_report_and_transitions() {
    let db = Database::open_in_memory().await.unwrap();
    let id = finished_run(&db, "system.noop", true).await;

    let (status, body) = call(&db, "GET", &format!("/api/v1/runs/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
    assert_eq!(body["state"], "succeeded");
    assert_eq!(body["report"]["counters"]["things"], 1);
    assert_eq!(body["report"]["expectation"]["status"], "met");
    let transitions = body["transitions"].as_array().unwrap();
    assert_eq!(transitions.len(), 2);
    assert_eq!(transitions[0]["from"], Value::Null);
    assert_eq!(transitions[0]["to"], "running");
    assert_eq!(transitions[1]["from"], "running");
    assert_eq!(transitions[1]["to"], "succeeded");
}

#[tokio::test]
async fn run_detail_of_unknown_run_is_404() {
    let db = Database::open_in_memory().await.unwrap();
    let (status, body) = call(&db, "GET", "/api/v1/runs/999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "runs.not_found");
}

#[tokio::test]
async fn trigger_enqueues_once_per_kind() {
    let db = Database::open_in_memory().await.unwrap();
    let (status, body) = call(&db, "POST", "/api/v1/jobs/system.noop/trigger").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job_id = body["jobId"].as_i64().unwrap();

    // A second trigger while the first is pending returns the same job.
    let (_, again) = call(&db, "POST", "/api/v1/jobs/system.noop/trigger").await;
    assert_eq!(again["jobId"].as_i64().unwrap(), job_id);

    let job = db
        .claim_job(
            "test",
            &[JobKind::Noop.name()],
            Utc::now(),
            Duration::from_secs(30),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.id, job_id);
    assert_eq!(job.trigger, Trigger::Manual);
}

#[tokio::test]
async fn trigger_of_unknown_kind_is_404() {
    let db = Database::open_in_memory().await.unwrap();
    let (status, _) = call(&db, "POST", "/api/v1/jobs/system.nope/trigger").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_lists_run_and_job_endpoints() {
    let db = Database::open_in_memory().await.unwrap();
    let (_, body) = call(&db, "GET", "/api/openapi.json").await;
    let paths = body["paths"].as_object().unwrap();
    for path in [
        "/api/v1/runs",
        "/api/v1/runs/{id}",
        "/api/v1/jobs/{kind}/trigger",
    ] {
        assert!(paths.contains_key(path), "{path} missing");
    }
}
