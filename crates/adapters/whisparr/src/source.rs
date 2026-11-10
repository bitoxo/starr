//! [`Source`] for Whisparr v3: scenes keyed by their `StashDB` UUID.
//!
//! Endpoints and their evidence: `docs/concept/capabilities.md`, section
//! "Radarr 6, Sonarr 4, Whisparr v3" (source read at `v3.1.0.2088` and
//! `v3.1.0.2116`). Paths are relative to `/api/v3`.
//!
//! The writes this adapter sends, and nothing else (spec 0007 Q16 A):
//! - `POST /movie` for a scene Whisparr does not have: `foreignId` (the
//!   `StashDB` UUID), `itemType: scene`, the title Whisparr's own lookup
//!   gave, the category's quality profile and root folder, `monitored:
//!   true`, `addOptions {monitor: movieOnly, searchForMovie}`;
//! - `PUT /movie/{id}`: the scene's resource as read, with only `monitored`
//!   switched false → true (extend) or true → false (stop);
//! - `POST /command {name: MoviesSearch, movieIds: [id]}` after extending.
//!
//! Never a write to `/performer` or `/studio`, never `movieAndCollection`
//! or another `monitor` value, never a delete, import list, indexer or
//! download client. Every write checks the version first, runs serialized
//! per scene and is read back through `GET /movie?stashId=`; a response
//! that is not the shape read in source fails as `Unknown` instead of
//! being guessed (spec 0007 Q2, accepted risk). Errors never name the
//! scene: neither its title nor its UUID.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use starr_adapter_kit::servarr::{Folder, Profile, QueueRecord};
use starr_core::adapters::{AdapterError, Listing, Probe};
use starr_core::source::{
    AddOutcome, AddRequest, Files, LibraryItem, Lookup, MonitorScope, NaturalKey, Progress,
    QualityProfile, QueueEntry, RootFolder, Source, WriteReport, check_added, check_extended,
    check_stopped,
};
use uuid::Uuid;

use crate::Whisparr;

/// `itemType` of a scene (`ItemType` enum, camelCase).
const SCENE: &str = "scene";

/// What a queue entry's message becomes: Whisparr's own words name the
/// release (private content).
const QUEUE_MESSAGE: &str = "Whisparr reports a message for this download; see its queue.";

/// The part of a `MovieResource` Starr reads. Every field is required
/// except those Whisparr fills only on some reads: a resource without them
/// is not the shape Starr knows.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Scene {
    id: i64,
    foreign_id: String,
    item_type: String,
    title: String,
    monitored: bool,
    quality_profile_id: i64,
    #[serde(default)]
    root_folder_path: Option<String>,
    #[serde(default)]
    has_file: Option<bool>,
}

impl Scene {
    /// The scene as it is before a write, to compare the read-back with;
    /// the comparison never needs the title, so it is left out.
    fn snapshot(&self, key: Uuid) -> LibraryItem {
        LibraryItem {
            remote_id: self.id,
            key: NaturalKey::StashScene(key),
            title: String::new(),
            monitored: self.monitored,
            quality_profile_id: self.quality_profile_id,
            root_folder_path: self.root_folder_path.clone().filter(|p| !p.is_empty()),
            units: Vec::new(),
        }
    }

    fn into_item(self, key: Uuid) -> LibraryItem {
        LibraryItem {
            remote_id: self.id,
            key: NaturalKey::StashScene(key),
            title: self.title,
            monitored: self.monitored,
            quality_profile_id: self.quality_profile_id,
            root_folder_path: self.root_folder_path.filter(|p| !p.is_empty()),
            units: Vec::new(),
        }
    }
}

/// A scene in the library: what Starr reads and the whole resource as
/// Whisparr sent it (for a read–modify–write).
#[derive(Debug)]
struct Found {
    scene: Scene,
    resource: Map<String, Value>,
}

/// `GET /lookup/scene` item (`SearchResource`): the scene is in `movie`.
#[derive(Debug, Deserialize)]
struct LookupResult {
    #[serde(default)]
    movie: Option<LookupScene>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LookupScene {
    foreign_id: String,
    item_type: String,
    title: String,
}

/// Whether `raw` (a `foreignId`) is the scene `id`. Whisparr keeps the
/// UUID as a string; a different spelling of the same UUID still matches.
fn is_scene(raw: &str, id: Uuid) -> bool {
    Uuid::parse_str(raw).is_ok_and(|parsed| parsed == id)
}

fn scene_id(key: NaturalKey) -> Result<Uuid, AdapterError> {
    match key {
        NaturalKey::StashScene(id) => Ok(id),
        NaturalKey::TmdbMovie(_) | NaturalKey::TvdbSeries(_) | NaturalKey::MbArtist(_) => Err(
            AdapterError::Rejected(format!("Whisparr keeps scenes by StashDB ID, not {key}")),
        ),
    }
}

fn scene_scope(scope: &MonitorScope) -> Result<(), AdapterError> {
    match scope {
        MonitorScope::Item => Ok(()),
        MonitorScope::Units(_) => Err(AdapterError::Rejected(
            "Whisparr scenes have no seasons or albums".to_owned(),
        )),
    }
}

impl Whisparr {
    /// The library entry of scene `id`, from `GET /movie?stashId=` (answered
    /// by `foreignId`, `MovieController.AllMovie`).
    async fn find(&self, id: Uuid) -> Result<Option<Found>, AdapterError> {
        let items: Vec<Value> = self
            .client
            .get_json(&format!("api/v3/movie?stashId={id}"))
            .await?;
        let mut matching = Vec::new();
        for item in items {
            let scene = Scene::deserialize(&item).map_err(|_| {
                AdapterError::Unknown(
                    "Whisparr's library entry is not the shape Starr knows".to_owned(),
                )
            })?;
            let Value::Object(resource) = item else {
                return Err(AdapterError::Unknown(
                    "Whisparr's library entry is not a JSON object".to_owned(),
                ));
            };
            if is_scene(&scene.foreign_id, id) {
                matching.push(Found { scene, resource });
            }
        }
        if matching.len() > 1 {
            return Err(AdapterError::Unknown(
                "Whisparr lists more than one entry with this StashDB ID".to_owned(),
            ));
        }
        let Some(found) = matching.pop() else {
            return Ok(None);
        };
        // A StashDB UUID names a scene; anything else is not what Starr
        // asked for (and nothing Starr may write to).
        if found.scene.item_type != SCENE {
            return Err(AdapterError::Unknown(
                "Whisparr keeps this StashDB ID as something other than a scene".to_owned(),
            ));
        }
        Ok(Some(found))
    }

    async fn find_required(&self, id: Uuid) -> Result<Found, AdapterError> {
        self.find(id)
            .await?
            .ok_or_else(|| AdapterError::NotFound("the scene is not in Whisparr".to_owned()))
    }

    /// The scene's title as Whisparr's own metadata knows it, from `GET
    /// /lookup/scene?term=stash:<uuid>` (`SearchController.SearchScene`).
    /// Whisparr needs a title to accept an add without a TMDB ID. An empty
    /// answer means its metadata server answered 404 for the UUID (or, per
    /// `SkyHookProxy.SearchForNewScene`, an HTML page); failures to reach
    /// it surface as 5xx and stay transient.
    async fn lookup_title(&self, id: Uuid) -> Result<String, AdapterError> {
        let results: Vec<LookupResult> = self
            .client
            .get_json(&format!("api/v3/lookup/scene?term=stash:{id}"))
            .await?;
        let scene = results
            .into_iter()
            .filter_map(|r| r.movie)
            .find(|m| is_scene(&m.foreign_id, id))
            .ok_or_else(|| {
                AdapterError::Rejected("Whisparr's metadata does not know this scene".to_owned())
            })?;
        if scene.item_type != SCENE || scene.title.trim().is_empty() {
            return Err(AdapterError::Unknown(
                "Whisparr's lookup for this scene is not the shape Starr knows".to_owned(),
            ));
        }
        Ok(scene.title)
    }

    /// Read–modify–write of the full resource as read: only `monitored`
    /// changes; profile, path, tags, files and everything else stay.
    async fn set_monitored(&self, found: Found, monitored: bool) -> Result<(), AdapterError> {
        let Found {
            scene,
            mut resource,
        } = found;
        resource.insert("monitored".into(), json!(monitored));
        self.client
            .put_json(&format!("api/v3/movie/{}", scene.id), &resource)
            .await
    }

    /// The scene as read back after a write.
    async fn read_back(&self, id: Uuid) -> Result<LibraryItem, AdapterError> {
        match self.find(id).await? {
            Some(found) => Ok(found.scene.into_item(id)),
            None => Err(AdapterError::Unknown(
                "Whisparr does not list the scene after the write".to_owned(),
            )),
        }
    }
}

/// The lock key of a scene: in memory only, never logged.
fn serial_key(id: Uuid) -> String {
    format!("stash:{id}")
}

impl Source for Whisparr {
    async fn quality_profiles(&self) -> Result<Listing<QualityProfile>, AdapterError> {
        let profiles: Vec<Profile> = self.client.get_json("api/v3/qualityprofile").await?;
        Ok(Listing {
            items: profiles.into_iter().map(QualityProfile::from).collect(),
            // Unpaged: the endpoint returns every profile.
            complete: true,
        })
    }

    async fn root_folders(&self) -> Result<Listing<RootFolder>, AdapterError> {
        let folders: Vec<Folder> = self.client.get_json("api/v3/rootfolder").await?;
        Ok(Listing {
            items: folders.into_iter().map(RootFolder::from).collect(),
            complete: true,
        })
    }

    async fn lookup(&self, key: NaturalKey) -> Result<Lookup, AdapterError> {
        let id = scene_id(key)?;
        Ok(match self.find(id).await? {
            Some(found) => Lookup::InLibrary(found.scene.into_item(id)),
            None => Lookup::NotInLibrary,
        })
    }

    async fn add(&self, request: &AddRequest) -> Result<AddOutcome, AdapterError> {
        let id = scene_id(request.key)?;
        scene_scope(&request.scope)?;
        // Writes only inside the tested version range (lesson 7, Q2).
        self.probe().await?;
        let _serial = self.client.serialize(&serial_key(id)).await;
        if let Some(existing) = self.find(id).await? {
            return Ok(AddOutcome::AlreadyExists(existing.scene.into_item(id)));
        }
        let title = self.lookup_title(id).await?;
        // Built field by field (Q16 A), not Whisparr's lookup posted back:
        // Whisparr reads its metadata from `foreignId` itself and applies
        // only path, profile, monitoring, root folder, tags and add options
        // from the body (`AddMovieService.AddSkyhookData`, `Movie.ApplyChanges`).
        let body = json!({
            "foreignId": id.to_string(),
            "itemType": SCENE,
            "title": title,
            "qualityProfileId": request.quality_profile_id,
            "rootFolderPath": request.root_folder_path,
            "monitored": true,
            "tags": [],
            "addOptions": { "monitor": "movieOnly", "searchForMovie": request.search },
        });
        let created: Value = match self.client.post_json("api/v3/movie", &body).await {
            Ok(created) => created,
            // "already added" (400) or a lost race: if the scene is there
            // now it exists. Whisparr's reason is withheld (private
            // content): it can name a studio or performer.
            Err(AdapterError::Rejected(reason)) => {
                return match self.find(id).await? {
                    Some(existing) => Ok(AddOutcome::AlreadyExists(existing.scene.into_item(id))),
                    None => Err(AdapterError::Rejected(reason)),
                };
            }
            Err(err) => return Err(err),
        };
        // The reply is not the result (lesson 9): read it back.
        let item = self.read_back(id).await?;
        if created.get("id").and_then(Value::as_i64) != Some(item.remote_id) {
            return Err(AdapterError::Unknown(
                "Whisparr lists another entry than the one it just added".to_owned(),
            ));
        }
        let deviations = check_added(request, &item);
        Ok(AddOutcome::Added(WriteReport {
            item,
            written: true,
            deviations,
        }))
    }

    async fn extend_monitoring(
        &self,
        key: NaturalKey,
        scope: &MonitorScope,
        search: bool,
    ) -> Result<WriteReport, AdapterError> {
        let id = scene_id(key)?;
        scene_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&serial_key(id)).await;
        let found = self.find_required(id).await?;
        let remote_id = found.scene.id;
        let (after, written, deviations) = if found.scene.monitored {
            (found.scene.into_item(id), false, Vec::new())
        } else {
            let before = found.scene.snapshot(id);
            self.set_monitored(found, true).await?;
            let after = self.read_back(id).await?;
            let deviations = check_extended(&before, scope, &after);
            (after, true, deviations)
        };
        // Search even when nothing had to be written: a retry after a
        // failed search must not lose it.
        if search && after.monitored {
            let _: Value = self
                .client
                .post_json(
                    "api/v3/command",
                    &json!({ "name": "MoviesSearch", "movieIds": [remote_id] }),
                )
                .await?;
        }
        Ok(WriteReport {
            item: after,
            written,
            deviations,
        })
    }

    async fn stop_monitoring(
        &self,
        key: NaturalKey,
        scope: &MonitorScope,
    ) -> Result<WriteReport, AdapterError> {
        let id = scene_id(key)?;
        scene_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&serial_key(id)).await;
        let found = self.find_required(id).await?;
        if !found.scene.monitored {
            return Ok(WriteReport {
                item: found.scene.into_item(id),
                written: false,
                deviations: Vec::new(),
            });
        }
        let before = found.scene.snapshot(id);
        // Only the scene: its performers and studio stay as they are, and
        // so do its file and queue (spec 0007, request removal).
        self.set_monitored(found, false).await?;
        let after = self.read_back(id).await?;
        let deviations = check_stopped(&before, scope, &after);
        Ok(WriteReport {
            item: after,
            written: true,
            deviations,
        })
    }

    async fn progress(&self, key: NaturalKey) -> Result<Progress, AdapterError> {
        let id = scene_id(key)?;
        let found = self.find_required(id).await?;
        let records: Vec<QueueRecord> = self
            .client
            .get_json(&format!("api/v3/queue/details?movieId={}", found.scene.id))
            .await?;
        // Starr's only evidence of a scene's availability (spec 0007 Q5 A):
        // a library entry without it is not the shape Starr knows, never
        // "no file" (that would remove available scenes, 0004 R4).
        let has_file = found.scene.has_file.ok_or_else(|| {
            AdapterError::Unknown(
                "Whisparr's library entry does not say whether it has the file".to_owned(),
            )
        })?;
        Ok(Progress {
            item: found.scene.into_item(id),
            files: Files::Movie { has_file },
            queue: Listing {
                items: records
                    .into_iter()
                    .map(QueueEntry::from)
                    .map(|mut entry| {
                        // Whisparr's queue messages quote release names
                        // (studio, scene): private content, so only a
                        // neutral hint that there is a message leaves the
                        // adapter (ADR 0014).
                        entry.message = entry.message.map(|_| QUEUE_MESSAGE.to_owned());
                        entry
                    })
                    .collect(),
                // Unpaged and filtered server-side: the scene's whole queue.
                complete: true,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_scene_keys_and_whole_item_scopes_are_whisparrs() {
        let id = Uuid::from_u128(0x5ce0_0000_0000_4000_8000_0000_0000_000a);
        assert_eq!(scene_id(NaturalKey::StashScene(id)).ok(), Some(id));
        for other in [
            NaturalKey::TmdbMovie(603),
            NaturalKey::TvdbSeries(81189),
            NaturalKey::MbArtist(id),
        ] {
            assert!(matches!(scene_id(other), Err(AdapterError::Rejected(_))));
        }
        assert!(scene_scope(&MonitorScope::Item).is_ok());
        assert!(matches!(
            scene_scope(&MonitorScope::seasons([1])),
            Err(AdapterError::Rejected(_))
        ));
    }

    /// Recorded from Whisparr 3.6.5.1886 (sanitised): every library entry
    /// reads as a `Scene` with `hasFile`, and the lookup of a scene the
    /// library has (`{foreignId, id, isExisting, movie}`, its `movie`
    /// without `hasFile` or `rootFolderPath`) still yields the title.
    #[test]
    fn recorded_entries_and_lookups_are_the_known_shape() {
        let all: Vec<Value> = serde_json::from_str(include_str!(
            "../fixtures/3.6.5.1886/recorded/movie_all.json"
        ))
        .unwrap_or_default();
        assert_eq!(all.len(), 3);
        for entry in &all {
            let scene = Scene::deserialize(entry).ok();
            assert!(scene.as_ref().is_some_and(|s| s.item_type == SCENE));
            assert!(scene.is_some_and(|s| s.has_file.is_some()));
        }
        let lookup: Vec<LookupResult> = serde_json::from_str(include_str!(
            "../fixtures/3.6.5.1886/recorded/lookup_scene_known.json"
        ))
        .unwrap_or_default();
        let id = Uuid::from_u128(0x5ce0_0002_0000_4000_8000_0000_0000_0002);
        let found = lookup
            .into_iter()
            .filter_map(|r| r.movie)
            .find(|m| is_scene(&m.foreign_id, id));
        assert_eq!(
            found.map(|m| (m.item_type, m.title)),
            Some((SCENE.to_owned(), "Scene D".to_owned()))
        );
    }

    #[test]
    fn a_foreign_id_matches_its_scene_in_any_spelling() {
        let id = Uuid::from_u128(0x5ce0_0000_0000_4000_8000_0000_0000_000a);
        assert!(is_scene("5ce00000-0000-4000-8000-00000000000a", id));
        assert!(is_scene("5CE00000-0000-4000-8000-00000000000A", id));
        assert!(!is_scene("5ce00000-0000-4000-8000-00000000000b", id));
        // A TMDB movie's foreignId is a number, never a scene.
        assert!(!is_scene("603", id));
    }
}
