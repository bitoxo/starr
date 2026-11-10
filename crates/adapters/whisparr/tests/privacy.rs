//! Spec 0007 (server logs) and ADR 0014: the Whisparr adapter logs and
//! reports no private content. Its own test binary: `tracing` caches
//! callsite interest per process, so a capturing subscriber next to tests
//! without one can miss events.
#![allow(clippy::unwrap_used)]
mod fake;

use fake::*;

// --- privacy ------------------------------------------------------------------

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Spec 0007 (server logs) and ADR 0014: submitting, extending, stopping
/// and failing at any of them logs no scene title, performer or studio
/// name and no `StashDB` UUID, at any level; the errors that reach run
/// reports and `last_error` carry none either.
#[tokio::test]
async fn submit_and_stop_paths_log_and_report_no_private_content() {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut errors: Vec<AdapterError> = Vec::new();
    let a = NaturalKey::StashScene(SCENE_A);

    // Submit: a new scene, an existing one, an unknown one, a rejected add.
    let fake = whisparr().await;
    let adapter = fake.adapter();
    assert!(matches!(
        adapter.add(&add_request(SCENE_B)).await,
        Ok(AddOutcome::Added(_))
    ));
    assert!(matches!(
        adapter.add(&add_request(SCENE_A)).await,
        Ok(AddOutcome::AlreadyExists(_))
    ));
    errors.push(adapter.add(&add_request(SCENE_C)).await.unwrap_err());
    let rejecting = whisparr().await;
    rejecting.state().reject_adds = true;
    errors.push(
        rejecting
            .adapter()
            .add(&add_request(SCENE_B))
            .await
            .unwrap_err(),
    );

    // Extend and stop, and a stop whose write fails.
    adapter
        .extend_monitoring(a, &MonitorScope::Item, true)
        .await
        .unwrap();
    adapter.progress(a).await.unwrap();
    adapter
        .stop_monitoring(a, &MonitorScope::Item)
        .await
        .unwrap();
    let failing = whisparr().await;
    failing.state().failing_puts = true;
    errors.push(
        failing
            .adapter()
            .extend_monitoring(a, &MonitorScope::Item, true)
            .await
            .unwrap_err(),
    );

    // A body that is not the known shape, and a scene that is not there.
    let mut broken = scene_a();
    broken.as_object_mut().unwrap().remove("itemType");
    let odd = fake_with(vec![broken], SUPPORTED).await;
    errors.push(
        odd.adapter()
            .stop_monitoring(a, &MonitorScope::Item)
            .await
            .unwrap_err(),
    );
    let empty = fake_with(Vec::new(), SUPPORTED).await;
    errors.push(
        empty
            .adapter()
            .stop_monitoring(a, &MonitorScope::Item)
            .await
            .unwrap_err(),
    );

    for err in &errors {
        let text = err.to_string();
        for marker in PRIVATE_MARKERS.iter().chain(&["Performer P"]) {
            assert!(!text.contains(marker), "the error names {marker:?}: {text}");
        }
    }
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("[private]") && logs.contains("service returned an error"),
        "nothing was logged, the check would be vacuous:\n{logs}"
    );
    for marker in PRIVATE_MARKERS.iter().chain(&["Performer P"]) {
        assert!(!logs.contains(marker), "the logs name {marker:?}:\n{logs}");
    }
}
