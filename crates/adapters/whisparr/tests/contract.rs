//! Whisparr v3 against the generic probe contract and its own `Source`
//! contract (spec 0007 slice 3, Q16 A).
//!
//! Reads come from `fixtures/3.6.5.1886/recorded/`: read-only `GET`s against
//! the maintainer's Whisparr 3.6.5.1886, sanitised (README there). What a
//! read-only recording cannot show — a scene Whisparr's metadata knows but
//! the library lacks, add answers, a queue with entries — is **derived from
//! Whisparr's source** (`fixtures/3.1.0.2116/`, README in `fixtures/`).
//! Placeholders only: "Scene A", "Scene B", "Performer P", "Studio S" and
//! made-up UUIDs.
//!
//! The fake (`fake/mod.rs`) is a small **stateful** Whisparr: a library of scenes
//! answered by `GET /movie?stashId=`, a metadata lookup that knows Scene B,
//! `POST /movie` that validates like `MovieController` and stores what
//! `AddMovieService` would, `PUT /movie/{id}` that applies what
//! `Movie.ApplyChanges` applies, and commands. It answers only requests with
//! the right key and records every write, including any write Starr must
//! never send (performers, studios, deletes, anything else).
#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod fake;

use fake::*;

/// What a queue message becomes (Whisparr's own words are private).
const NEUTRAL_QUEUE_MESSAGE: &str = "Whisparr reports a message for this download; see its queue.";

// --- probe ------------------------------------------------------------------

#[tokio::test]
async fn whisparr_passes_the_probe_contract() {
    run_probe_contract(&ProbeContract {
        descriptor: &DESCRIPTOR,
        auth: AUTH,
        probe_method: "GET",
        probe_path: "/api/v3/system/status",
        supported: vec![contract::Fixture {
            version: SUPPORTED.parse().unwrap(),
            body: status_body(SUPPORTED),
        }],
        unsupported: contract::Fixture {
            version: UNSUPPORTED.parse().unwrap(),
            body: status_body(UNSUPPORTED),
        },
        // `ApiKeyAuthenticationHandler` as in Radarr: an empty 401.
        unauthorized: RecordedResponse {
            status: 401,
            body: String::new(),
        },
        build: Whisparr::new,
    })
    .await;
}

#[tokio::test]
async fn another_arr_app_at_the_url_is_not_accepted_as_whisparr() {
    let server = MockServer::start().await;
    let body = status_body(SUPPORTED).replace("\"Whisparr\"", "\"Radarr\"");
    Mock::given(path("/api/v3/system/status"))
        .and(header("X-Api-Key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    let result = Whisparr::new(client(&server.uri())).probe().await;
    assert!(
        matches!(&result, Err(AdapterError::Unknown(msg)) if msg.contains("Radarr")),
        "{result:?}"
    );
}

#[test]
fn the_tested_range_is_v3_from_its_first_tag() {
    assert_eq!(DESCRIPTOR.type_id, "whisparr");
    let range = DESCRIPTOR.tested_versions;
    for inside in ["3.1.0.2088", "3.1.0.2116", SUPPORTED, "3.9.0.0"] {
        assert!(range.contains(&inside.parse().unwrap()), "{inside}");
    }
    for outside in ["3.1.0.2087", "4.0.0.0", "2.2.0.231"] {
        assert!(!range.contains(&outside.parse().unwrap()), "{outside}");
    }
    // Whisparr holds private content: the client logs no path and no
    // words of the service (ADR 0014).
    assert!(client_options().private_content);
    assert_eq!(client_options().rate_limit, DESCRIPTOR.budgets.rate_limit);
}

// --- lists and lookup -------------------------------------------------------

#[tokio::test]
async fn lists_profiles_and_root_folders() {
    let fake = whisparr().await;
    let adapter = fake.adapter();
    assert_eq!(
        adapter.quality_profiles().await,
        Ok(Listing {
            items: vec![
                QualityProfile {
                    id: 4,
                    name: "1080p".into()
                },
                QualityProfile {
                    id: 5,
                    name: "4K".into()
                },
            ],
            complete: true
        })
    );
    // An inaccessible root folder is the shared `servarr::Folder` mapping,
    // covered by the Radarr and Sonarr contracts.
    assert_eq!(
        adapter.root_folders().await,
        Ok(Listing {
            items: vec![RootFolder {
                id: 2,
                path: "/data/private".into(),
                accessible: true,
                free_space_bytes: Some(2_698_666_250_240),
            }],
            complete: true
        })
    );
    assert_eq!(fake.state().writes(), 0);
}

#[tokio::test]
async fn lookup_finds_a_scene_by_its_stashdb_id_only() {
    let fake = whisparr().await;
    let adapter = fake.adapter();
    match adapter.lookup(NaturalKey::StashScene(SCENE_A)).await {
        Ok(Lookup::InLibrary(item)) => {
            assert_eq!(item.remote_id, SCENE_A_ID);
            assert_eq!(item.key, NaturalKey::StashScene(SCENE_A));
            assert!(!item.monitored);
            assert_eq!(item.quality_profile_id, PROFILE);
            assert_eq!(item.root_folder_path.as_deref(), Some("/data/private"));
            assert_eq!(item.units, Vec::new());
        }
        other => panic!("expected Scene A, got {other:?}"),
    }
    assert_eq!(
        adapter.lookup(NaturalKey::StashScene(SCENE_B)).await,
        Ok(Lookup::NotInLibrary)
    );
    // Whisparr keeps scenes only.
    assert!(matches!(
        adapter.lookup(NaturalKey::TmdbMovie(603)).await,
        Err(AdapterError::Rejected(_))
    ));
    assert_eq!(fake.state().writes(), 0);
}

/// The three recorded library states (Whisparr 3.6.5.1886): unmonitored
/// without file, monitored with file, monitored without file; each read
/// through `GET /movie?stashId=` and its own (recorded, empty) queue.
#[tokio::test]
async fn the_recorded_library_states_read_as_recorded() {
    let states = [
        ("unmonitored", false, false),
        ("with_file", true, true),
        ("monitored_no_file", true, false),
    ];
    let scenes: Vec<Value> = states
        .iter()
        .map(|(name, _, _)| recorded_json(&format!("movie_by_stashid_{name}.json"))[0].clone())
        .collect();
    let fake = fake_with(scenes.clone(), SUPPORTED).await;
    for ((name, monitored, has_file), scene) in states.iter().zip(&scenes) {
        let id: Uuid = scene["foreignId"].as_str().unwrap().parse().unwrap();
        let remote_id = scene["id"].as_i64().unwrap();
        Mock::given(method("GET"))
            .and(path("/api/v3/queue/details"))
            .and(query_param("movieId", remote_id.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(recorded(&format!("queue_details_{name}.json"))),
            )
            .with_priority(1)
            .mount(&fake.server)
            .await;
        let progress = fake
            .adapter()
            .progress(NaturalKey::StashScene(id))
            .await
            .unwrap();
        assert_eq!(progress.item.remote_id, remote_id, "{name}");
        assert_eq!(progress.item.monitored, *monitored, "{name}");
        assert_eq!(progress.item.quality_profile_id, PROFILE, "{name}");
        assert_eq!(
            progress.files,
            Files::Movie {
                has_file: *has_file
            },
            "{name}"
        );
        assert_eq!(progress.queue.items, Vec::new(), "{name}");
        assert!(progress.queue.complete, "{name}");
    }
    // Every entry Whisparr listed is a scene (`itemType: scene`), and the
    // full listing holds exactly the entries `?stashId=` answered.
    let all = recorded_json("movie_all.json");
    assert!(
        all.as_array()
            .unwrap()
            .iter()
            .all(|m| m["itemType"] == "scene")
    );
    for scene in &scenes {
        assert!(all.as_array().unwrap().contains(scene));
    }
    // An unknown StashDB UUID: `[]`, not in the library.
    assert_eq!(recorded_json("movie_by_stashid_unknown.json"), json!([]));
    assert_eq!(
        fake.adapter().lookup(NaturalKey::StashScene(SCENE_C)).await,
        Ok(Lookup::NotInLibrary)
    );
    assert_eq!(fake.state().writes(), 0);
}

/// Recorded: Whisparr keeps performers and studios of the library as its
/// own entries; none of the performers is monitored, and the operator's
/// studio monitoring is his (Starr never writes either, Q16 A).
#[test]
fn recorded_performers_and_studios_are_left_as_the_operator_set_them() {
    for performer in recorded_json("performer_all.json").as_array().unwrap() {
        assert_eq!(performer["monitored"], json!(false));
        assert_eq!(performer["moviesMonitored"], json!(false));
    }
    for studio in recorded_json("studio_all.json").as_array().unwrap() {
        assert_eq!(studio["moviesMonitored"], json!(false));
        assert!(studio["monitored"].is_boolean());
    }
}

#[tokio::test]
async fn an_entry_that_is_not_a_scene_or_not_the_known_shape_is_unknown() {
    // The same UUID kept as something else than a scene: nothing to write.
    let mut not_a_scene = scene_a();
    not_a_scene["itemType"] = json!("movie");
    let fake = fake_with(vec![not_a_scene], SUPPORTED).await;
    let adapter = fake.adapter();
    assert!(matches!(
        adapter.lookup(NaturalKey::StashScene(SCENE_A)).await,
        Err(AdapterError::Unknown(_))
    ));
    assert!(matches!(
        adapter
            .extend_monitoring(NaturalKey::StashScene(SCENE_A), &MonitorScope::Item, true)
            .await,
        Err(AdapterError::Unknown(_))
    ));
    assert_eq!(fake.state().writes(), 0);

    // A future 3.x build without `itemType` (or `monitored`): not guessed.
    for field in ["itemType", "monitored", "qualityProfileId"] {
        let mut changed = scene_a();
        changed.as_object_mut().unwrap().remove(field);
        let fake = fake_with(vec![changed], SUPPORTED).await;
        let adapter = fake.adapter();
        assert!(
            matches!(
                adapter.lookup(NaturalKey::StashScene(SCENE_A)).await,
                Err(AdapterError::Unknown(_))
            ),
            "{field}"
        );
        assert!(matches!(
            adapter
                .stop_monitoring(NaturalKey::StashScene(SCENE_A), &MonitorScope::Item)
                .await,
            Err(AdapterError::Unknown(_))
        ));
        assert_eq!(fake.state().writes(), 0, "{field}");
    }
}

// --- add --------------------------------------------------------------------

#[tokio::test]
async fn add_posts_exactly_the_scene_and_reads_it_back() {
    let fake = whisparr().await;
    let outcome = fake.adapter().add(&add_request(SCENE_B)).await;
    match &outcome {
        Ok(AddOutcome::Added(report)) => {
            assert!(report.written);
            assert_eq!(report.deviations, vec![]);
            assert_eq!(report.item.remote_id, SCENE_B_ID);
            assert_eq!(report.item.key, NaturalKey::StashScene(SCENE_B));
            assert!(report.item.monitored);
            assert_eq!(report.item.quality_profile_id, PROFILE);
        }
        other => panic!("expected Added, got {other:?}"),
    }
    let state = fake.state();
    assert_eq!(state.posts.len(), 1, "exactly one add");
    // Exactly these fields (Q16 A): no performer or studio monitoring, no
    // `movieAndCollection`, nothing from the lookup posted back.
    assert_eq!(
        state.posts[0],
        json!({
            "foreignId": SCENE_B.to_string(),
            "itemType": "scene",
            "title": "Scene B",
            "qualityProfileId": PROFILE,
            "rootFolderPath": "/data/private",
            "monitored": true,
            "tags": [],
            "addOptions": { "monitor": "movieOnly", "searchForMovie": true },
        })
    );
    // The search is Whisparr's own post-add action (`searchForMovie`).
    assert!(state.puts.is_empty() && state.commands.is_empty());
    assert_eq!(state.forbidden, Vec::<String>::new());
}

#[tokio::test]
async fn add_without_search_says_so() {
    let fake = whisparr().await;
    let request = AddRequest {
        search: false,
        ..add_request(SCENE_B)
    };
    assert!(matches!(
        fake.adapter().add(&request).await,
        Ok(AddOutcome::Added(_))
    ));
    assert_eq!(
        fake.state().posts[0]["addOptions"],
        json!({ "monitor": "movieOnly", "searchForMovie": false })
    );
}

#[tokio::test]
async fn add_of_a_scene_in_the_library_changes_nothing() {
    let fake = whisparr().await;
    match fake.adapter().add(&add_request(SCENE_A)).await {
        Ok(AddOutcome::AlreadyExists(item)) => {
            assert_eq!(item.remote_id, SCENE_A_ID);
            assert!(!item.monitored, "left as it is");
        }
        other => panic!("expected AlreadyExists, got {other:?}"),
    }
    assert_eq!(fake.state().writes(), 0);
}

#[tokio::test]
async fn concurrent_adds_post_once() {
    let fake = whisparr().await;
    let shared = client(&fake.server.uri());
    let (first, second) = (Whisparr::new(shared.clone()), Whisparr::new(shared));
    let request = add_request(SCENE_B);
    let (a, b) = tokio::join!(first.add(&request), second.add(&request));
    let added = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(AddOutcome::Added(_))))
        .count();
    let existed = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(AddOutcome::AlreadyExists(_))))
        .count();
    assert_eq!((added, existed), (1, 1), "{a:?} / {b:?}");
    assert_eq!(fake.state().posts.len(), 1, "adds must be serialized");
}

#[tokio::test]
async fn the_duplicate_answer_means_already_exists() {
    let fake = whisparr().await;
    // Someone else added Scene B between our lookup and our add.
    {
        let mut state = fake.state();
        let mut b = derived_json("lookup_scene_b.json")[0]["movie"].clone();
        b["id"] = json!(SCENE_B_ID);
        b["rootFolderPath"] = json!("/data/private");
        b["qualityProfileId"] = json!(1);
        state.scenes.push(b);
        state.hidden = true;
    }
    match fake.adapter().add(&add_request(SCENE_B)).await {
        Ok(AddOutcome::AlreadyExists(item)) => assert_eq!(item.remote_id, SCENE_B_ID),
        other => panic!("expected AlreadyExists, got {other:?}"),
    }
    let state = fake.state();
    assert_eq!(state.posts.len(), 1);
    assert!(state.puts.is_empty(), "an existing scene is left untouched");
}

#[tokio::test]
async fn the_read_back_reports_what_whisparr_stored() {
    let fake = whisparr().await;
    fake.state().mangle = true;
    let Ok(AddOutcome::Added(report)) = fake.adapter().add(&add_request(SCENE_B)).await else {
        panic!("expected Added");
    };
    assert_eq!(
        report.deviations,
        vec![Deviation::QualityProfile {
            expected: PROFILE,
            found: MANGLED_PROFILE
        }]
    );

    // An exclusion with a tag: Whisparr adds the scene unmonitored.
    let fake = whisparr().await;
    fake.state().store_unmonitored = true;
    let Ok(AddOutcome::Added(report)) = fake.adapter().add(&add_request(SCENE_B)).await else {
        panic!("expected Added");
    };
    assert_eq!(report.deviations, vec![Deviation::ItemNotMonitored]);
    // Starr does not fight it: one add, nothing else.
    let state = fake.state();
    assert_eq!((state.posts.len(), state.writes()), (1, 1));
}

#[tokio::test]
async fn a_scene_whisparrs_metadata_does_not_know_is_rejected_without_a_write() {
    let fake = whisparr().await;
    let result = fake.adapter().add(&add_request(SCENE_C)).await;
    assert!(
        matches!(&result, Err(AdapterError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(fake.state().writes(), 0);
}

#[tokio::test]
async fn a_rejected_add_is_not_retried_and_names_nothing() {
    let fake = whisparr().await;
    fake.state().reject_adds = true;
    match fake.adapter().add(&add_request(SCENE_B)).await {
        Err(AdapterError::Rejected(reason)) => {
            // Whisparr's reason names the studio: private content keeps it
            // out of Starr (ADR 0014).
            assert!(!reason.contains("Studio S"), "{reason}");
            assert!(reason.contains("HTTP 400"), "{reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert_eq!(fake.state().posts.len(), 1, "no retry of a rejected add");
    assert_eq!(fake.state().writes(), 1);
}

#[tokio::test]
async fn a_server_error_on_the_add_stays_transient() {
    let fake = whisparr().await;
    Mock::given(method("POST"))
        .and(path("/api/v3/movie"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&fake.server)
        .await;
    let result = fake.adapter().add(&add_request(SCENE_B)).await;
    assert!(
        matches!(result, Err(AdapterError::Transient(_))),
        "{result:?}"
    );
}

// --- extend and stop ----------------------------------------------------------

#[tokio::test]
async fn extend_only_switches_the_scene_on_and_searches() {
    let fake = whisparr().await;
    let adapter = fake.adapter();
    let key = NaturalKey::StashScene(SCENE_A);
    let report = adapter
        .extend_monitoring(key, &MonitorScope::Item, true)
        .await
        .unwrap();
    assert!(report.written);
    assert_eq!(report.deviations, vec![]);
    assert!(report.item.monitored);
    {
        let state = fake.state();
        assert_eq!(state.puts.len(), 1, "one read–modify–write");
        let put = &state.puts[0];
        assert_eq!(put["monitored"], json!(true));
        assert_eq!(
            without_monitored(put),
            without_monitored(&scene_a()),
            "only `monitored` may change"
        );
        assert_eq!(
            state.commands,
            vec![json!({ "name": "MoviesSearch", "movieIds": [SCENE_A_ID] })]
        );
        assert_eq!(state.posts, Vec::<Value>::new());
        assert_eq!(state.forbidden, Vec::<String>::new());
    }
    // Asking again writes nothing; the search is asked for again (a retry
    // after a failed search must not lose it).
    let again = adapter
        .extend_monitoring(key, &MonitorScope::Item, true)
        .await
        .unwrap();
    assert!(!again.written);
    let state = fake.state();
    assert_eq!(state.puts.len(), 1);
    assert_eq!(state.commands.len(), 2);
}

#[tokio::test]
async fn extend_without_search_sends_no_command() {
    let fake = whisparr().await;
    fake.adapter()
        .extend_monitoring(NaturalKey::StashScene(SCENE_A), &MonitorScope::Item, false)
        .await
        .unwrap();
    let state = fake.state();
    assert_eq!((state.puts.len(), state.commands.len()), (1, 0));
}

#[tokio::test]
async fn a_failed_search_is_repeated_on_retry() {
    let fake = whisparr().await;
    fake.state().failing_commands = 1;
    let adapter = fake.adapter();
    let key = NaturalKey::StashScene(SCENE_A);
    let first = adapter
        .extend_monitoring(key, &MonitorScope::Item, true)
        .await;
    assert!(
        matches!(first, Err(AdapterError::Transient(_))),
        "{first:?}"
    );
    let retry = adapter
        .extend_monitoring(key, &MonitorScope::Item, true)
        .await
        .unwrap();
    assert!(!retry.written);
    let state = fake.state();
    assert_eq!(state.puts.len(), 1);
    assert_eq!(state.commands.len(), 1, "the search was lost");
}

#[tokio::test]
async fn stop_unmonitors_only_the_scene_and_searches_nothing() {
    let fake = whisparr().await;
    let adapter = fake.adapter();
    let key = NaturalKey::StashScene(SCENE_A);
    adapter
        .extend_monitoring(key, &MonitorScope::Item, false)
        .await
        .unwrap();
    let report = adapter
        .stop_monitoring(key, &MonitorScope::Item)
        .await
        .unwrap();
    assert!(report.written);
    assert_eq!(report.deviations, vec![]);
    assert!(!report.item.monitored);
    {
        let state = fake.state();
        assert_eq!(state.puts.len(), 2);
        assert_eq!(
            state.puts[1],
            scene_a(),
            "stopping turns off exactly what extending turned on"
        );
        assert!(state.posts.is_empty() && state.commands.is_empty());
        assert_eq!(state.forbidden, Vec::<String>::new());
    }
    let again = adapter
        .stop_monitoring(key, &MonitorScope::Item)
        .await
        .unwrap();
    assert!(!again.written);
    assert_eq!(fake.state().puts.len(), 2);
}

#[tokio::test]
async fn units_are_refused_before_anything_is_sent() {
    let fake = whisparr().await;
    let adapter = fake.adapter();
    let key = NaturalKey::StashScene(SCENE_A);
    let seasons = MonitorScope::seasons([1]);
    assert!(matches!(
        adapter.extend_monitoring(key, &seasons, true).await,
        Err(AdapterError::Rejected(_))
    ));
    assert!(matches!(
        adapter.stop_monitoring(key, &seasons).await,
        Err(AdapterError::Rejected(_))
    ));
    let request = AddRequest {
        scope: seasons,
        ..add_request(SCENE_B)
    };
    assert!(matches!(
        adapter.add(&request).await,
        Err(AdapterError::Rejected(_))
    ));
    assert_eq!(fake.state().writes(), 0);
}

// --- progress, absence, failures, versions ----------------------------------

/// `hasFile` is Starr's only evidence of a scene's availability (spec 0007
/// Q5 A): an entry without it is an unknown shape, never "no file".
#[tokio::test]
async fn progress_without_has_file_is_unknown_not_absent() {
    let mut scene = scene_a();
    scene.as_object_mut().unwrap().remove("hasFile");
    let fake = fake_with(vec![scene], SUPPORTED).await;
    assert!(matches!(
        fake.adapter()
            .progress(NaturalKey::StashScene(SCENE_A))
            .await,
        Err(AdapterError::Unknown(_))
    ));
}

#[tokio::test]
async fn progress_maps_queue_and_files() {
    let fake = whisparr().await;
    let progress = fake
        .adapter()
        .progress(NaturalKey::StashScene(SCENE_A))
        .await
        .unwrap();
    assert_eq!(progress.item.remote_id, SCENE_A_ID);
    assert_eq!(progress.files, Files::Movie { has_file: false });
    assert_eq!(
        progress.queue,
        Listing {
            items: vec![
                QueueEntry {
                    unit: None,
                    size_bytes: 1_500_000_000,
                    size_left_bytes: 500_000_000,
                    state: QueueState::Downloading,
                    message: None,
                },
                QueueEntry {
                    unit: None,
                    size_bytes: 800_000_000,
                    size_left_bytes: 0,
                    state: QueueState::Warning,
                    // Whisparr's words can name the release: never passed on.
                    message: Some(NEUTRAL_QUEUE_MESSAGE.into()),
                },
            ],
            complete: true
        }
    );

    // A scene with its file.
    let mut with_file = scene_a();
    with_file["hasFile"] = json!(true);
    let fake = fake_with(vec![with_file], SUPPORTED).await;
    let progress = fake
        .adapter()
        .progress(NaturalKey::StashScene(SCENE_A))
        .await
        .unwrap();
    assert_eq!(progress.files, Files::Movie { has_file: true });
}

#[tokio::test]
async fn absent_scenes_are_not_found_and_nothing_is_written() {
    let fake = fake_with(Vec::new(), SUPPORTED).await;
    let adapter = fake.adapter();
    let key = NaturalKey::StashScene(SCENE_A);
    assert!(matches!(
        adapter.progress(key).await,
        Err(AdapterError::NotFound(_))
    ));
    assert!(matches!(
        adapter
            .extend_monitoring(key, &MonitorScope::Item, true)
            .await,
        Err(AdapterError::NotFound(_))
    ));
    assert!(matches!(
        adapter.stop_monitoring(key, &MonitorScope::Item).await,
        Err(AdapterError::NotFound(_))
    ));
    assert_eq!(fake.state().writes(), 0);
}

#[tokio::test]
async fn failures_follow_the_taxonomy() {
    type Expect = fn(&AdapterError) -> bool;
    let cases: [(ResponseTemplate, Expect); 4] = [
        (ResponseTemplate::new(401), |e| {
            *e == AdapterError::CredentialsInvalid
        }),
        (
            ResponseTemplate::new(429).insert_header("retry-after", "5"),
            |e| {
                *e == AdapterError::Throttled {
                    retry_after: Some(Duration::from_secs(5)),
                }
            },
        ),
        (ResponseTemplate::new(500), |e| {
            matches!(e, AdapterError::Transient(_))
        }),
        (ResponseTemplate::new(503), |e| {
            matches!(e, AdapterError::Transient(_))
        }),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(response)
            .mount(&server)
            .await;
        let result = Whisparr::new(client(&server.uri()))
            .lookup(NaturalKey::StashScene(SCENE_A))
            .await;
        assert!(
            result.as_ref().err().is_some_and(expected),
            "lookup: {result:?}"
        );
    }
}

#[tokio::test]
async fn outside_the_tested_range_whisparr_is_read_only() {
    for version in [UNSUPPORTED, "4.0.0.0"] {
        let fake = if version == UNSUPPORTED {
            fake_with(vec![scene_a()], UNSUPPORTED).await
        } else {
            let server_fake = fake_with(vec![scene_a()], SUPPORTED).await;
            Mock::given(method("GET"))
                .and(path("/api/v3/system/status"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(status_body(SUPPORTED).replace(SUPPORTED, version)),
                )
                .with_priority(1)
                .mount(&server_fake.server)
                .await;
            server_fake
        };
        let adapter = fake.adapter();
        let key = NaturalKey::StashScene(SCENE_A);
        assert!(matches!(
            adapter.lookup(key).await,
            Ok(Lookup::InLibrary(_))
        ));
        assert!(adapter.progress(key).await.is_ok(), "reads keep working");
        assert!(matches!(
            adapter
                .extend_monitoring(key, &MonitorScope::Item, true)
                .await,
            Err(AdapterError::VersionUnsupported { .. })
        ));
        assert!(matches!(
            adapter.stop_monitoring(key, &MonitorScope::Item).await,
            Err(AdapterError::VersionUnsupported { .. })
        ));
        assert!(matches!(
            adapter.add(&add_request(SCENE_B)).await,
            Err(AdapterError::VersionUnsupported { .. })
        ));
        assert_eq!(fake.state().writes(), 0, "{version}");
    }
}

/// `progress` reads the queue of the scene's own id, nothing broader.
#[tokio::test]
async fn the_queue_is_read_for_the_scene_only() {
    let fake = whisparr().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/queue/details"))
        .and(query_param("movieId", SCENE_A_ID.to_string()))
        .respond_with(json_response(200, &json!([])))
        .with_priority(1)
        .expect(1)
        .mount(&fake.server)
        .await;
    let progress = fake
        .adapter()
        .progress(NaturalKey::StashScene(SCENE_A))
        .await
        .unwrap();
    assert_eq!(progress.queue.items, Vec::new());
}

/// A queue entry's `errorMessage`, or a status message with only a title,
/// quotes the release name: only the neutral hint leaves the adapter.
#[tokio::test]
async fn queue_messages_never_carry_whisparrs_words() {
    let fake = whisparr().await;
    let mut records = derived_json("queue_details.json");
    records[0]["errorMessage"] = json!("Studio.S.Scene.A.1080p failed");
    records[1]["statusMessages"] = json!([{ "title": "Studio.S.Scene.A.720p", "messages": [] }]);
    Mock::given(method("GET"))
        .and(path("/api/v3/queue/details"))
        .respond_with(json_response(200, &records))
        .with_priority(1)
        .mount(&fake.server)
        .await;
    let progress = fake
        .adapter()
        .progress(NaturalKey::StashScene(SCENE_A))
        .await
        .unwrap();
    assert_eq!(progress.queue.items.len(), 2);
    for entry in &progress.queue.items {
        assert_eq!(entry.message.as_deref(), Some(NEUTRAL_QUEUE_MESSAGE));
    }
}

/// A 3.x build that ignored `stashId` and listed the whole library: only
/// the entry with the scene's `foreignId` counts, and only it is written.
#[tokio::test]
async fn a_library_listed_whole_is_filtered_by_foreign_id() {
    let mut other = derived_json("lookup_scene_b.json")[0]["movie"].clone();
    other["id"] = json!(SCENE_B_ID);
    other["monitored"] = json!(true);
    other["qualityProfileId"] = json!(1);
    let fake = fake_with(vec![other, scene_a()], SUPPORTED).await;
    fake.state().ignore_stash_filter = true;
    let report = fake
        .adapter()
        .extend_monitoring(NaturalKey::StashScene(SCENE_A), &MonitorScope::Item, false)
        .await
        .unwrap();
    assert_eq!(report.item.remote_id, SCENE_A_ID);
    assert_eq!(report.deviations, vec![]);
    let state = fake.state();
    assert_eq!(state.puts.len(), 1);
    assert_eq!(state.puts[0]["id"], json!(SCENE_A_ID));
    assert_eq!(state.forbidden, Vec::<String>::new());
}

#[tokio::test]
async fn two_entries_with_one_stashdb_id_are_unknown_and_untouched() {
    let mut twin = scene_a();
    twin["id"] = json!(13);
    let fake = fake_with(vec![scene_a(), twin], SUPPORTED).await;
    fake.state().ignore_stash_filter = true;
    let adapter = fake.adapter();
    let key = NaturalKey::StashScene(SCENE_A);
    assert!(matches!(
        adapter
            .extend_monitoring(key, &MonitorScope::Item, true)
            .await,
        Err(AdapterError::Unknown(_))
    ));
    assert!(matches!(
        adapter.add(&add_request(SCENE_A)).await,
        Err(AdapterError::Unknown(_))
    ));
    assert_eq!(fake.state().writes(), 0);
}

#[tokio::test]
async fn an_add_reply_naming_another_entry_is_unknown() {
    let fake = whisparr().await;
    fake.state().reply_id = Some(99);
    let result = fake.adapter().add(&add_request(SCENE_B)).await;
    assert!(
        matches!(&result, Err(AdapterError::Unknown(_))),
        "{result:?}"
    );
    // One add, nothing written on top of it.
    let state = fake.state();
    assert_eq!((state.posts.len(), state.writes()), (1, 1));
}

#[tokio::test]
async fn a_failed_write_sends_no_search() {
    let fake = whisparr().await;
    fake.state().failing_puts = true;
    let result = fake
        .adapter()
        .extend_monitoring(NaturalKey::StashScene(SCENE_A), &MonitorScope::Item, true)
        .await;
    assert!(
        matches!(result, Err(AdapterError::Transient(_))),
        "{result:?}"
    );
    let state = fake.state();
    assert_eq!(state.puts.len(), 1);
    assert_eq!(state.commands, Vec::<Value>::new());
}
