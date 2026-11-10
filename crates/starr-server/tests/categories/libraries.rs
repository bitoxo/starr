//! Libraries per category (spec 0005 slice 2): admins map an instance's
//! libraries to categories (one category per library and instance), see
//! unmapped libraries and what each member would get; members see none of
//! it; nothing is ever written to the service.
use super::common::media::member;
use super::common::services::{Env, env};
use super::common::{Req, get, post};
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{any, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "emby-libraries-key-0123456789";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../adapters/emby/fixtures/4.10.1.0/accounts/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn put(path: &str, body: Value) -> Req {
    Req::new(Method::PUT, path, Some(body))
}

fn patch(path: &str, body: Value) -> Req {
    Req::new(Method::PATCH, path, Some(body))
}

/// An Emby from recorded fixtures (Movies `3`, Shows `5`) plus an "Adult"
/// library `12`; answers `GET`s with the key and records everything else.
async fn fake_emby() -> MockServer {
    let server = MockServer::start().await;
    let mut folders: Vec<Value> = serde_json::from_str(&fixture("virtual_folders.json")).unwrap();
    folders.push(json!({
        "Name": "Adult", "ItemId": "12", "Guid": "aa11bb22cc33dd44ee55ff6677889900",
        "CollectionType": "movies", "Locations": ["/media/adult"]
    }));
    for (route, body) in [
        ("/System/Info", fixture("system_info.json")),
        ("/Users", fixture("users.json")),
        ("/Library/VirtualFolders", Value::Array(folders).to_string()),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("x-emby-token", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
    }
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    server
}

/// The Emby as consume instance for movies, series and adult (all switched
/// on), scanned once. Returns the service id and the role ids by category.
async fn scanned(env: &Env, server: &MockServer) -> (i64, Value) {
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "emby", "name": "Emby", "baseUrl": server.uri(), "apiKey": KEY }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    let id = res.body["service"]["id"].as_i64().unwrap();
    let mut roles = json!({});
    for category in ["movies", "series", "adult"] {
        let res = env
            .send(patch(
                &format!("/api/v1/categories/{category}"),
                json!({ "enabled": true, "operatorStatement": true }),
            ))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        let res = env
            .send(post(
                &format!("/api/v1/categories/{category}/roles"),
                json!({ "kind": "consume", "serviceId": id }),
            ))
            .await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.raw);
        roles[category] = res.body["consumers"][0]["id"].clone();
    }
    let res = env
        .send(post("/api/v1/jobs/accounts.scan/trigger", json!({})))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.raw);
    env.drain().await;
    (id, roles)
}

async fn map(env: &Env, category: &str, role: &Value, ids: &[&str]) -> super::common::TestResponse {
    env.send(put(
        &format!("/api/v1/categories/{category}/roles/{role}/libraries"),
        json!({ "libraryIds": ids }),
    ))
    .await
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

fn unmapped_names(body: &Value) -> Vec<&str> {
    body.as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn admins_map_libraries_once_per_instance_and_see_unmapped_ones() {
    let env = env().await;
    let server = fake_emby().await;
    let (id, roles) = scanned(&env, &server).await;

    let res = env
        .send(get(&format!("/api/v1/services/{id}/libraries")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["complete"], true);
    let names: Vec<_> = res.body["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["id"].as_str().unwrap(), l["category"].clone()))
        .collect();
    assert_eq!(
        names,
        [("12", Value::Null), ("3", Value::Null), ("5", Value::Null)]
    );
    let res = env.send(get("/api/v1/libraries/unmapped")).await;
    assert_eq!(unmapped_names(&res.body), ["Adult", "Movies", "Shows"]);

    let res = map(&env, "movies", &roles["movies"], &["3"]).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(
        res.body["consumers"][0]["libraries"],
        json!([{ "id": "3", "name": "Movies" }])
    );
    // The adult library cannot also be a movies library, nor the other way.
    let res = map(&env, "adult", &roles["adult"], &["12", "3"]).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.raw);
    assert_eq!(res.code(), "categories.library_overlap");
    let res = map(&env, "adult", &roles["adult"], &["999"]).await;
    assert_eq!(res.code(), "categories.library_unknown");
    let many = vec!["3"; 201];
    let res = map(&env, "adult", &roles["adult"], &many).await;
    assert_eq!(res.code(), "validation.too_many");
    // A role of another category is not found under this one.
    let res = map(&env, "series", &roles["adult"], &["5"]).await;
    assert_eq!(res.code(), "categories.role_not_found");
    let res = map(&env, "adult", &roles["adult"], &["12"]).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let res = env.send(get("/api/v1/categories")).await;
    let movies = res
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "movies")
        .unwrap();
    assert_eq!(movies["consumers"][0]["libraries"][0]["id"], "3");

    let res = env
        .send(get(&format!("/api/v1/services/{id}/libraries")))
        .await;
    let adult = res.body["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == "12")
        .unwrap();
    assert_eq!(adult["category"], "adult");
    let res = env.send(get("/api/v1/libraries/unmapped")).await;
    assert_eq!(unmapped_names(&res.body), ["Shows"]);

    // Ignoring a library takes it off the list, audited once.
    let url = format!("/api/v1/services/{id}/libraries/5/ignored");
    let res = env.send(put(&url, json!({ "ignored": true }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert_eq!(res.body["ignoredBy"], "root");
    env.send(put(&url, json!({ "ignored": true }))).await;
    let res = env.send(get("/api/v1/libraries/unmapped")).await;
    assert_eq!(res.body, json!([]));
    let res = env
        .send(put(
            &format!("/api/v1/services/{id}/libraries/nope/ignored"),
            json!({ "ignored": true }),
        ))
        .await;
    assert_eq!(res.code(), "libraries.not_found");

    let (actions,): (String,) = sqlx::query_as(
        "SELECT group_concat(action, ',') FROM
           (SELECT action FROM audit_log
            WHERE action IN ('categories.libraries', 'libraries.ignore') ORDER BY id)",
    )
    .fetch_one(env.app.state.db.pool())
    .await
    .unwrap();
    assert_eq!(
        actions,
        "categories.libraries,categories.libraries,libraries.ignore"
    );
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn members_get_their_categories_libraries_and_adult_only_when_entitled() {
    let env = env().await;
    let server = fake_emby().await;
    let (_, roles) = scanned(&env, &server).await;
    map(&env, "movies", &roles["movies"], &["3"]).await;
    map(&env, "adult", &roles["adult"], &["12"]).await;
    let ent = |category: &str| json!({ "category": category, "enabled": true, "requestMode": "approval" });
    let alex = member(&env, "alex", json!([ent("movies"), ent("series")])).await;
    let sam = member(&env, "sam", json!([ent("movies"), ent("adult")])).await;

    let res = env
        .send(get(&format!("/api/v1/members/{}/libraries", alex.id)))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    let emby = &res.body[0];
    assert_eq!(emby["serviceName"], "Emby");
    assert_eq!(
        emby["libraries"],
        json!([{ "id": "3", "name": "Movies", "category": "movies" }])
    );
    // Series has a consume role here without libraries: nothing yet.
    assert_eq!(emby["unmappedCategories"], json!(["series"]));

    let res = env
        .send(get(&format!("/api/v1/members/{}/libraries", sam.id)))
        .await;
    let ids: Vec<_> = res.body[0]["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["12", "3"], "sorted by grant id: the Guids");
    assert_eq!(res.body[0]["unmappedCategories"], json!([]));

    // Adult switched off as a category: nobody gets it, whatever the
    // entitlement says.
    env.send(patch(
        "/api/v1/categories/adult",
        json!({ "enabled": false }),
    ))
    .await;
    let res = env
        .send(get(&format!("/api/v1/members/{}/libraries", sam.id)))
        .await;
    assert_eq!(
        res.body[0]["libraries"],
        json!([{ "id": "3", "name": "Movies", "category": "movies" }])
    );

    let res = env.send(get("/api/v1/members/999/libraries")).await;
    assert_eq!(res.code(), "members.not_found");
    assert_eq!(non_get_requests(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn members_never_see_libraries_or_mappings() {
    let env = env().await;
    let server = fake_emby().await;
    let (id, roles) = scanned(&env, &server).await;
    let alex = member(&env, "alex", json!([])).await;
    for req in [
        get(&format!("/api/v1/services/{id}/libraries")),
        get("/api/v1/libraries/unmapped"),
        get(&format!("/api/v1/members/{}/libraries", alex.id)),
        put(
            &format!("/api/v1/services/{id}/libraries/12/ignored"),
            json!({ "ignored": true }),
        ),
        put(
            &format!(
                "/api/v1/categories/adult/roles/{}/libraries",
                roles["adult"]
            ),
            json!({ "libraryIds": ["12"] }),
        ),
    ] {
        let res = req.cookie(&alex.cookie).send(&env.app).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.raw);
        assert!(!res.raw.contains("Adult"), "{}", res.raw);
    }
}

#[tokio::test]
async fn only_consume_roles_of_library_services_take_libraries() {
    let env = env().await;
    let res = env
        .send(post(
            "/api/v1/services",
            json!({ "adapterType": "radarr", "name": "Radarr", "baseUrl": "http://radarr:7878", "apiKey": "k" }),
        ))
        .await;
    let radarr = res.body["service"]["id"].as_i64().unwrap();
    let res = env
        .send(get(&format!("/api/v1/services/{radarr}/libraries")))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.code(), "libraries.unsupported");
    let res = env
        .send(post(
            "/api/v1/categories/movies/roles",
            json!({ "kind": "source", "serviceId": radarr }),
        ))
        .await;
    let source = res.body["source"]["id"].clone();
    let res = map(&env, "movies", &source, &[]).await;
    assert_eq!(res.code(), "categories.not_a_consumer");
    let res = env.send(get("/api/v1/services/999/libraries")).await;
    assert_eq!(res.code(), "services.not_found");
}
