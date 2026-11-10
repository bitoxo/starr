//! Response shapes the Servarr family (Radarr, Sonarr, Lidarr; later
//! Whisparr) shares, so each adapter does not repeat their mapping. Evidence:
//! `docs/concept/capabilities.md`, section "Radarr 6, Sonarr 4, Whisparr v3".

use serde::Deserialize;
use starr_core::source::{QualityProfile, QueueEntry, QueueState, RootFolder, UnitId};

/// The readable reason in a Servarr error body: the `errorMessage`s of a
/// validation failure (an array, or one object), else an error object's
/// `message`. `None` when the body has neither, e.g. it is not JSON.
#[must_use]
pub fn error_messages(body: &[u8]) -> Option<String> {
    fn of(value: &serde_json::Value) -> Option<String> {
        ["errorMessage", "message"]
            .iter()
            .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
            // One line for the UI and reports (Sonarr's messages carry
            // newlines).
            .map(|m| m.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|m| !m.is_empty())
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let messages: Vec<String> = match &value {
        serde_json::Value::Array(items) => items.iter().filter_map(of).collect(),
        object @ serde_json::Value::Object(_) => of(object).into_iter().collect(),
        _ => Vec::new(),
    };
    let mut unique: Vec<String> = Vec::new();
    for message in messages {
        if !unique.contains(&message) {
            unique.push(message);
        }
    }
    (!unique.is_empty()).then(|| unique.join("; "))
}

/// `GET /api/v3/qualityprofile` (Lidarr `/api/v1`) item (only what Starr reads).
#[derive(Debug, Deserialize)]
pub struct Profile {
    id: i64,
    name: String,
}

impl From<Profile> for QualityProfile {
    fn from(p: Profile) -> Self {
        Self {
            id: p.id,
            name: p.name,
        }
    }
}

/// `GET /api/v3/rootfolder` (Lidarr `/api/v1`) item (only what Starr reads).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    id: i64,
    path: String,
    #[serde(default)]
    accessible: bool,
    #[serde(default)]
    free_space: Option<u64>,
}

impl From<Folder> for RootFolder {
    fn from(f: Folder) -> Self {
        Self {
            id: f.id,
            path: f.path,
            accessible: f.accessible,
            free_space_bytes: f.free_space,
        }
    }
}

#[derive(Debug, Deserialize)]
struct StatusMessage {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    messages: Vec<String>,
}

/// `GET /api/v3/queue/details` item (only what Starr reads).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueRecord {
    /// Sonarr only.
    #[serde(default)]
    season_number: Option<u32>,
    /// Lidarr only: the album's id in Lidarr (mapped to its release group
    /// by the adapter).
    #[serde(default)]
    album_id: Option<i64>,
    #[serde(default)]
    size: f64,
    #[serde(default)]
    sizeleft: f64,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tracked_download_status: Option<String>,
    #[serde(default)]
    tracked_download_state: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    status_messages: Vec<StatusMessage>,
}

impl QueueRecord {
    /// Lidarr: the id of the album the download belongs to.
    #[must_use]
    pub const fn album_id(&self) -> Option<i64> {
        self.album_id
    }
}

/// Map `status` (`QueueStatus`), `trackedDownloadStatus` and
/// `trackedDownloadState` (camelCase enum names) to a [`QueueState`].
fn queue_state(
    status: Option<&str>,
    tracked_status: Option<&str>,
    tracked_state: Option<&str>,
) -> QueueState {
    let troubled = matches!(tracked_status, Some("warning" | "error"));
    match tracked_state {
        Some("failed" | "failedPending") => return QueueState::Failed,
        Some("importing") => return QueueState::Importing,
        // Imported but still listed: done, not pending.
        Some("imported" | "ignored") => return QueueState::Other,
        Some("importPending" | "importBlocked") if troubled => return QueueState::Warning,
        Some("importPending" | "importBlocked") => return QueueState::ImportPending,
        _ => {}
    }
    match status {
        Some("queued" | "delay") => QueueState::Queued,
        Some("paused") => QueueState::Paused,
        Some("downloading") if troubled => QueueState::Warning,
        Some("downloading") => QueueState::Downloading,
        Some("completed") => QueueState::ImportPending,
        Some("failed") => QueueState::Failed,
        Some("warning" | "downloadClientUnavailable") => QueueState::Warning,
        _ => QueueState::Other,
    }
}

/// Queue sizes are JSON decimals.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn bytes(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

impl From<QueueRecord> for QueueEntry {
    fn from(record: QueueRecord) -> Self {
        let state = queue_state(
            record.status.as_deref(),
            record.tracked_download_status.as_deref(),
            record.tracked_download_state.as_deref(),
        );
        let message = record.error_message.filter(|m| !m.is_empty()).or_else(|| {
            let parts: Vec<String> = record
                .status_messages
                .into_iter()
                .flat_map(|m| {
                    if m.messages.is_empty() {
                        m.title.into_iter().collect()
                    } else {
                        m.messages
                    }
                })
                .filter(|m| !m.is_empty())
                .collect();
            (!parts.is_empty()).then(|| parts.join("; "))
        });
        Self {
            unit: record.season_number.map(UnitId::Season),
            size_bytes: bytes(record.size),
            size_left_bytes: bytes(record.sizeleft),
            state,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_states_map_from_status_and_tracked_state() {
        let s = |status, tracked, state| queue_state(Some(status), tracked, state);
        let cases = [
            (
                ("downloading", Some("ok"), Some("downloading")),
                QueueState::Downloading,
            ),
            (
                ("downloading", Some("warning"), Some("downloading")),
                QueueState::Warning,
            ),
            (
                ("completed", Some("ok"), Some("importPending")),
                QueueState::ImportPending,
            ),
            (
                ("completed", Some("warning"), Some("importBlocked")),
                QueueState::Warning,
            ),
            (
                ("completed", Some("ok"), Some("importing")),
                QueueState::Importing,
            ),
            (
                ("failed", Some("error"), Some("failedPending")),
                QueueState::Failed,
            ),
            (("queued", None, None), QueueState::Queued),
            (("delay", None, None), QueueState::Queued),
            (
                ("paused", Some("ok"), Some("downloading")),
                QueueState::Paused,
            ),
            (("fallback", None, None), QueueState::Other),
            (
                ("completed", Some("ok"), Some("imported")),
                QueueState::Other,
            ),
        ];
        for ((status, tracked, state), expected) in cases {
            assert_eq!(
                s(status, tracked, state),
                expected,
                "{status} {tracked:?} {state:?}"
            );
        }
    }

    #[test]
    fn decimal_sizes_become_bytes() {
        assert_eq!(bytes(1_500_000_000.0), 1_500_000_000);
        assert_eq!(bytes(-1.0), 0);
        assert_eq!(bytes(f64::NAN), 0);
    }

    fn fixture(path: &str) -> Vec<u8> {
        std::fs::read(format!("{}/../adapters/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn error_bodies_yield_their_messages() {
        // Radarr validation failure (array of `ValidationFailure`).
        assert_eq!(
            error_messages(&fixture(
                "radarr/fixtures/6.4.4.10685/movie_add_duplicate_400.json"
            ))
            .as_deref(),
            Some("This movie has already been added")
        );
        // Sonarr error object (`message`, `description`).
        assert_eq!(
            error_messages(&fixture(
                "sonarr/fixtures/4.0.20.3014/series_add_conflict_409.json"
            ))
            .as_deref(),
            Some("constraint failed UNIQUE constraint failed: Series.TvdbId")
        );
        // A single validation object, repeated messages, and non-JSON.
        assert_eq!(
            error_messages(br#"{"propertyName": "Path", "errorMessage": "Path is not writable"}"#)
                .as_deref(),
            Some("Path is not writable")
        );
        assert_eq!(
            error_messages(
                br#"[{"errorMessage": "a"}, {"errorMessage": "a"}, {"errorMessage": "b"}]"#
            )
            .as_deref(),
            Some("a; b")
        );
        assert_eq!(error_messages(b"<html>Bad Request</html>"), None);
        assert_eq!(error_messages(br#"[{"errorMessage": ""}]"#), None);
    }

    #[test]
    fn messages_fall_back_from_error_to_status_messages() {
        let record: QueueRecord = serde_json::from_str(
            r#"{"size": 100, "sizeleft": 40, "status": "completed",
                "trackedDownloadStatus": "warning", "trackedDownloadState": "importBlocked",
                "errorMessage": "", "seasonNumber": 2,
                "statusMessages": [{"title": "x.mkv", "messages": ["No files found are eligible for import"]},
                                   {"title": "Only a title", "messages": []}]}"#,
        )
        .unwrap();
        let entry = QueueEntry::from(record);
        assert_eq!(entry.unit, Some(UnitId::Season(2)));
        assert_eq!(entry.state, QueueState::Warning);
        assert_eq!(entry.size_left_bytes, 40);
        assert_eq!(
            entry.message.as_deref(),
            Some("No files found are eligible for import; Only a title")
        );
    }
}
