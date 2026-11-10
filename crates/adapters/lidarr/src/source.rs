//! [`Source`] for Lidarr: artists keyed by `MusicBrainz` artist MBID,
//! monitored per album (release group, spec 0006 Q4 A).
//!
//! What Starr writes, and nothing else (spec 0006 Q5 A, Q13 A, 0004 R1):
//! - `POST /album` with the album and its artist embedded: for an artist
//!   not in Lidarr yet the artist gets the category's profiles and folder,
//!   `monitored: true`, `monitorNewItems: "none"` and `addOptions.monitor:
//!   "none"`, so only the requested album is monitored; for an artist
//!   already there the artist is embedded exactly as read (Lidarr keeps
//!   using it, its settings stay untouched);
//! - `PUT /album/monitor` with `monitored: true` for albums Lidarr has but
//!   does not monitor, and `monitored: false` to stop exactly those again;
//! - `POST /command` `AlbumSearch` for the requested albums.
//!
//! The artist itself is never written (no `PUT`/`DELETE /artist`), nothing
//! is deleted. Endpoints and their evidence: `docs/concept/capabilities.md`,
//! section "Lidarr".

use std::collections::{BTreeSet, HashMap};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use starr_adapter_kit::servarr::{Folder, Profile, QueueRecord};
use starr_core::adapters::{AdapterError, Listing, Probe};
use starr_core::source::{
    AddOutcome, AddRequest, Files, LibraryItem, Lookup, MetadataProfile, MonitorScope, NaturalKey,
    Progress, QualityProfile, QueueEntry, RootFolder, Source, UnitId, UnitMonitoring, UnitProgress,
    WriteReport, check_added, check_extended, check_stopped,
};
use uuid::Uuid;

use crate::Lidarr;

/// The part of an `ArtistResource` Starr reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Artist {
    id: i64,
    #[serde(rename = "artistName")]
    name: String,
    monitored: bool,
    quality_profile_id: i64,
    #[serde(default)]
    root_folder_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Statistics {
    #[serde(default)]
    track_file_count: u32,
    /// Tracks of the monitored release that are wanted or have a file.
    #[serde(default)]
    track_count: u32,
}

/// The part of an `AlbumResource` Starr reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Album {
    id: i64,
    foreign_album_id: String,
    monitored: bool,
    #[serde(default)]
    statistics: Option<Statistics>,
}

impl Album {
    /// The release group, if Lidarr's id is one (it always is for albums
    /// from `MusicBrainz`; anything else is skipped).
    fn release_group(&self) -> Option<Uuid> {
        Uuid::parse_str(&self.foreign_album_id).ok()
    }

    /// Every track of the monitored release has a file: nothing to search.
    fn complete(&self) -> bool {
        self.statistics
            .as_ref()
            .is_some_and(|s| s.track_count > 0 && s.track_file_count >= s.track_count)
    }
}

/// `GET /api/v1/metadataprofile` item.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataProfileResource {
    id: i64,
    name: String,
    #[serde(default)]
    primary_album_types: Vec<TypeItem>,
    #[serde(default)]
    secondary_album_types: Vec<TypeItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TypeItem {
    album_type: TypeName,
    allowed: bool,
}

#[derive(Debug, Deserialize)]
struct TypeName {
    name: String,
}

fn allowed(items: Vec<TypeItem>) -> Vec<String> {
    items
        .into_iter()
        .filter(|i| i.allowed)
        .map(|i| i.album_type.name)
        .collect()
}

impl From<MetadataProfileResource> for MetadataProfile {
    fn from(p: MetadataProfileResource) -> Self {
        Self {
            id: p.id,
            name: p.name,
            primary_types: allowed(p.primary_album_types),
            secondary_types: allowed(p.secondary_album_types),
        }
    }
}

/// An artist as Lidarr has it, with its albums.
struct InLidarr {
    artist: Artist,
    /// The resource as read, embedded unchanged in album adds.
    raw: Map<String, Value>,
    albums: Vec<Album>,
}

impl InLidarr {
    fn item(&self, mbid: Uuid) -> LibraryItem {
        LibraryItem {
            remote_id: self.artist.id,
            key: NaturalKey::MbArtist(mbid),
            title: self.artist.name.clone(),
            monitored: self.artist.monitored,
            quality_profile_id: self.artist.quality_profile_id,
            root_folder_path: self
                .artist
                .root_folder_path
                .clone()
                .filter(|p| !p.is_empty()),
            units: self
                .albums
                .iter()
                .filter_map(|a| {
                    Some(UnitMonitoring {
                        unit: UnitId::ReleaseGroup(a.release_group()?),
                        monitored: a.monitored,
                    })
                })
                .collect(),
        }
    }

    fn album(&self, release_group: Uuid) -> Option<&Album> {
        self.albums
            .iter()
            .find(|a| a.release_group() == Some(release_group))
    }
}

fn mb_artist(key: NaturalKey) -> Result<Uuid, AdapterError> {
    match key {
        NaturalKey::MbArtist(id) => Ok(id),
        NaturalKey::TmdbMovie(_) | NaturalKey::TvdbSeries(_) | NaturalKey::StashScene(_) => Err(
            AdapterError::Rejected(format!("Lidarr keeps artists by MusicBrainz ID, not {key}")),
        ),
    }
}

fn album_scope(scope: &MonitorScope) -> Result<BTreeSet<Uuid>, AdapterError> {
    let rejected = || {
        AdapterError::Rejected("Lidarr artists are monitored per album; name the albums".to_owned())
    };
    match scope {
        MonitorScope::Units(units) if !units.is_empty() => units
            .iter()
            .map(|unit| match unit {
                UnitId::ReleaseGroup(id) => Ok(*id),
                UnitId::Season(_) => Err(rejected()),
            })
            .collect(),
        MonitorScope::Units(_) | MonitorScope::Item => Err(rejected()),
    }
}

/// Whether a JSON id field names `mbid`.
fn names(value: Option<&Value>, mbid: Uuid) -> bool {
    value
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        == Some(mbid)
}

impl Lidarr {
    /// The artist with MBID `mbid` and its albums, if Lidarr has it.
    /// `GET /artist?mbId=` answers `[]` for an unknown MBID (recorded,
    /// 3.1.6). Only entries whose `foreignArtistId` is `mbid` count, so an
    /// ignored filter cannot hand back another artist.
    async fn find(&self, mbid: Uuid) -> Result<Option<InLidarr>, AdapterError> {
        let found: Vec<Value> = self
            .client
            .get_json(&format!("api/v1/artist?mbId={mbid}"))
            .await?;
        let mut matching = found.into_iter().filter_map(|a| match a {
            Value::Object(a) if names(a.get("foreignArtistId"), mbid) => Some(Value::Object(a)),
            _ => None,
        });
        let first = matching.next();
        if matching.next().is_some() {
            return Err(AdapterError::Unknown(format!(
                "Lidarr lists more than one artist with MusicBrainz ID {mbid}"
            )));
        }
        let Some(value) = first else {
            return Ok(None);
        };
        let artist = Artist::deserialize(&value)
            .map_err(|e| AdapterError::Unknown(format!("unexpected artist resource: {e}")))?;
        let Value::Object(raw) = value else {
            return Err(AdapterError::Unknown(
                "unexpected artist resource".to_owned(),
            ));
        };
        let albums: Vec<Album> = self
            .client
            .get_json(&format!("api/v1/album?artistId={}", artist.id))
            .await?;
        Ok(Some(InLidarr {
            artist,
            raw,
            albums,
        }))
    }

    async fn find_required(&self, mbid: Uuid) -> Result<InLidarr, AdapterError> {
        self.find(mbid)
            .await?
            .ok_or_else(|| AdapterError::NotFound(format!("mbartist:{mbid} is not in Lidarr")))
    }

    /// Lidarr's metadata for release group `release_group` of artist `mbid`
    /// (`GET /album/lookup?term=lidarr:<mbid>`), the body of an add.
    async fn album_metadata(
        &self,
        mbid: Uuid,
        release_group: Uuid,
    ) -> Result<Map<String, Value>, AdapterError> {
        let results: Vec<Map<String, Value>> = self
            .client
            .get_json(&format!(
                "api/v1/album/lookup?term=lidarr%3A{release_group}"
            ))
            .await?;
        let album = results
            .into_iter()
            .find(|r| names(r.get("foreignAlbumId"), release_group))
            .ok_or_else(|| {
                AdapterError::NotFound(format!(
                    "Lidarr's metadata does not know album {release_group}"
                ))
            })?;
        let artist = album.get("artist").and_then(|a| a.get("foreignArtistId"));
        if !names(artist, mbid) {
            return Err(AdapterError::Rejected(format!(
                "album {release_group} is not by artist {mbid} in Lidarr's metadata"
            )));
        }
        Ok(album)
    }

    /// `POST /album`: the album from Lidarr's metadata with `artist`
    /// embedded, monitored, optionally searched right away.
    async fn post_album(
        &self,
        mut album: Map<String, Value>,
        artist: Map<String, Value>,
        search: bool,
    ) -> Result<(), AdapterError> {
        album.insert("artist".into(), Value::Object(artist));
        album.insert("monitored".into(), json!(true));
        album.insert("addOptions".into(), json!({ "searchForNewAlbum": search }));
        let _: Value = self.client.post_json("api/v1/album", &album).await?;
        Ok(())
    }

    async fn set_monitored(
        &self,
        album_ids: Vec<i64>,
        monitored: bool,
    ) -> Result<(), AdapterError> {
        self.client
            .put_json(
                "api/v1/album/monitor",
                &json!({ "albumIds": album_ids, "monitored": monitored }),
            )
            .await
    }

    async fn search(&self, album_ids: Vec<i64>) -> Result<(), AdapterError> {
        let _: Value = self
            .client
            .post_json(
                "api/v1/command",
                &json!({ "name": "AlbumSearch", "albumIds": album_ids }),
            )
            .await?;
        Ok(())
    }

    /// Monitor `wanted` under the artist as `found`: albums Lidarr has but
    /// does not monitor are switched on, albums it does not have (not
    /// refreshed yet, or outside the artist's metadata profile) are added
    /// with the artist embedded as read. With `search` the requested albums
    /// are searched. Returns the artist as read back and whether anything
    /// was written.
    async fn monitor(
        &self,
        mbid: Uuid,
        found: InLidarr,
        wanted: &BTreeSet<Uuid>,
        search: bool,
    ) -> Result<(InLidarr, bool), AdapterError> {
        // Albums Lidarr has but does not monitor: switch them on. Albums it
        // does not have (e.g. not refreshed yet, or outside the artist's
        // metadata profile): add them under the artist as it is.
        let switch_on: Vec<i64> = wanted
            .iter()
            .filter_map(|&rg| found.album(rg))
            .filter(|a| !a.monitored)
            .map(|a| a.id)
            .collect();
        let missing: Vec<Uuid> = wanted
            .iter()
            .copied()
            .filter(|&rg| found.album(rg).is_none())
            .collect();
        let mut written = false;
        if !switch_on.is_empty() {
            self.set_monitored(switch_on, true).await?;
            written = true;
        }
        let artist_monitored = found.artist.monitored;
        let mut raw = Some(found.raw);
        for &release_group in &missing {
            let album = self.album_metadata(mbid, release_group).await?;
            let artist = match raw.take() {
                Some(artist) => artist,
                None => self.find_required(mbid).await?.raw,
            };
            // Searched with the add: Lidarr searches a new album itself.
            self.post_album(album, artist, search && artist_monitored)
                .await?;
            written = true;
        }

        let after = if written {
            self.find_required(mbid).await?
        } else {
            InLidarr {
                artist: found.artist,
                raw: Map::new(),
                albums: found.albums,
            }
        };
        // Search the requested albums that were there already, also when
        // nothing had to be written: a retry after a failed search must
        // not lose it. Albums with every track on disk are there already
        // (no write at all for them, spec 0006 acceptance).
        if search && after.artist.monitored {
            let ids: Vec<i64> = wanted
                .iter()
                .filter(|rg| !missing.contains(rg))
                .filter_map(|&rg| after.album(rg))
                .filter(|a| a.monitored && !a.complete())
                .map(|a| a.id)
                .collect();
            if !ids.is_empty() {
                self.search(ids).await?;
            }
        }
        Ok((after, written))
    }
}

/// The embedded artist of an add that creates it: Lidarr's metadata with
/// the category's settings, monitoring only what Starr adds (Q5 A).
/// `albumsToMonitor` names the requested albums: when Lidarr applies the
/// artist's add options after its first scan, a non-empty list monitors
/// exactly those instead of applying `monitor: "none"` to every album.
fn new_artist(
    mut artist: Map<String, Value>,
    request: &AddRequest,
    metadata_profile_id: i64,
    albums: &BTreeSet<Uuid>,
) -> Map<String, Value> {
    artist.insert("qualityProfileId".into(), json!(request.quality_profile_id));
    artist.insert("metadataProfileId".into(), json!(metadata_profile_id));
    artist.insert("rootFolderPath".into(), json!(request.root_folder_path));
    artist.insert("monitored".into(), json!(true));
    // New albums are Starr's business (follows): without this Lidarr would
    // pick them up by itself, unrequested and unattributed.
    artist.insert("monitorNewItems".into(), json!("none"));
    artist.insert(
        "addOptions".into(),
        json!({
            "monitor": "none",
            "albumsToMonitor": albums.iter().map(Uuid::to_string).collect::<Vec<_>>(),
            "searchForMissingAlbums": false,
        }),
    );
    artist
}

impl Source for Lidarr {
    async fn quality_profiles(&self) -> Result<Listing<QualityProfile>, AdapterError> {
        let profiles: Vec<Profile> = self.client.get_json("api/v1/qualityprofile").await?;
        Ok(Listing {
            items: profiles.into_iter().map(QualityProfile::from).collect(),
            // Unpaged: the endpoint returns every profile.
            complete: true,
        })
    }

    async fn root_folders(&self) -> Result<Listing<RootFolder>, AdapterError> {
        let folders: Vec<Folder> = self.client.get_json("api/v1/rootfolder").await?;
        Ok(Listing {
            items: folders.into_iter().map(RootFolder::from).collect(),
            complete: true,
        })
    }

    async fn metadata_profiles(&self) -> Result<Listing<MetadataProfile>, AdapterError> {
        let profiles: Vec<MetadataProfileResource> =
            self.client.get_json("api/v1/metadataprofile").await?;
        Ok(Listing {
            items: profiles.into_iter().map(MetadataProfile::from).collect(),
            complete: true,
        })
    }

    async fn lookup(&self, key: NaturalKey) -> Result<Lookup, AdapterError> {
        let mbid = mb_artist(key)?;
        Ok(match self.find(mbid).await? {
            Some(found) => Lookup::InLibrary(found.item(mbid)),
            None => Lookup::NotInLibrary,
        })
    }

    async fn add(&self, request: &AddRequest) -> Result<AddOutcome, AdapterError> {
        let mbid = mb_artist(request.key)?;
        let wanted = album_scope(&request.scope)?;
        let metadata_profile_id = request.metadata_profile_id.ok_or_else(|| {
            AdapterError::Rejected(
                "Lidarr needs a metadata profile for a new artist; pick one in the category's source settings"
                    .to_owned(),
            )
        })?;
        // Writes only inside the tested version range (lesson 7).
        self.probe().await?;
        // Two album adds of one new artist could create it twice: one add
        // per artist MBID at a time (spec 0006 Q5).
        let _serial = self.client.serialize(&request.key.to_string()).await;
        if let Some(existing) = self.find(mbid).await? {
            return Ok(AddOutcome::AlreadyExists(existing.item(mbid)));
        }

        let mut rest = wanted.iter().copied();
        let Some(first) = rest.next() else {
            return Err(AdapterError::Rejected("no album to add".to_owned()));
        };
        let mut album = self.album_metadata(mbid, first).await?;
        let Some(Value::Object(metadata)) = album.remove("artist") else {
            return Err(AdapterError::Unknown(format!(
                "Lidarr's metadata for album {first} has no artist"
            )));
        };
        let artist = new_artist(metadata, request, metadata_profile_id, &wanted);
        match self.post_album(album, artist, request.search).await {
            Ok(()) => {}
            // "already added" or a lost race: if the artist is there now, it
            // exists and is left untouched.
            Err(AdapterError::Rejected(reason)) => {
                return match self.find(mbid).await? {
                    Some(existing) => Ok(AddOutcome::AlreadyExists(existing.item(mbid))),
                    None => Err(AdapterError::Rejected(reason)),
                };
            }
            Err(err) => return Err(err),
        }
        // Further albums go under the artist that exists now; Lidarr may
        // already list them (unmonitored) after its refresh.
        let rest: BTreeSet<Uuid> = rest.collect();
        if !rest.is_empty() {
            let found = self.find_required(mbid).await?;
            self.monitor(mbid, found, &rest, request.search).await?;
        }
        // The reply is not the result (lesson 9): read it back.
        let item = self.find_required(mbid).await?.item(mbid);
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
        let mbid = mb_artist(key)?;
        let wanted = album_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&key.to_string()).await;
        let found = self.find_required(mbid).await?;
        let before = found.item(mbid);

        let (after, written) = self.monitor(mbid, found, &wanted, search).await?;
        let after_item = after.item(mbid);
        let deviations = check_extended(&before, scope, &after_item);
        Ok(WriteReport {
            item: after_item,
            written,
            deviations,
        })
    }

    async fn stop_monitoring(
        &self,
        key: NaturalKey,
        scope: &MonitorScope,
    ) -> Result<WriteReport, AdapterError> {
        let mbid = mb_artist(key)?;
        let stopped = album_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&key.to_string()).await;
        let found = self.find_required(mbid).await?;
        let before = found.item(mbid);
        let ids: Vec<i64> = stopped
            .iter()
            .filter_map(|&rg| found.album(rg))
            .filter(|a| a.monitored)
            .map(|a| a.id)
            .collect();
        if ids.is_empty() {
            return Ok(WriteReport {
                item: before,
                written: false,
                deviations: Vec::new(),
            });
        }
        // Only these albums go off; the artist stays as it is (Q13 A), its
        // other albums, files and the queue are untouched.
        self.set_monitored(ids, false).await?;
        let after = self.find_required(mbid).await?.item(mbid);
        let deviations = check_stopped(&before, scope, &after);
        Ok(WriteReport {
            item: after,
            written: true,
            deviations,
        })
    }

    async fn progress(&self, key: NaturalKey) -> Result<Progress, AdapterError> {
        let mbid = mb_artist(key)?;
        let found = self.find_required(mbid).await?;
        let records: Vec<QueueRecord> = self
            .client
            .get_json(&format!(
                "api/v1/queue/details?artistId={}",
                found.artist.id
            ))
            .await?;
        let units: Vec<UnitProgress> = found
            .albums
            .iter()
            .filter_map(|album| {
                let stats = album.statistics.as_ref();
                let tracks = stats.map_or(0, |s| s.track_count);
                let files = stats.map_or(0, |s| s.track_file_count);
                Some(UnitProgress {
                    unit: UnitId::ReleaseGroup(album.release_group()?),
                    monitored: album.monitored,
                    expected: tracks,
                    expected_with_file: files.min(tracks),
                    with_file: files,
                })
            })
            .collect();
        let release_groups: HashMap<i64, Uuid> = found
            .albums
            .iter()
            .filter_map(|a| Some((a.id, a.release_group()?)))
            .collect();
        let queue = records
            .into_iter()
            .map(|record| {
                let unit = record
                    .album_id()
                    .and_then(|id| release_groups.get(&id))
                    .map(|&rg| UnitId::ReleaseGroup(rg));
                QueueEntry {
                    unit,
                    ..QueueEntry::from(record)
                }
            })
            .collect();
        Ok(Progress {
            item: found.item(mbid),
            // The album list is unpaged per artist; the queue details are
            // unpaged and filtered server-side by artist.
            files: Files::Units(Listing {
                items: units,
                complete: true,
            }),
            queue: Listing {
                items: queue,
                complete: true,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_artist_monitors_nothing_but_what_starr_adds() {
        let request = AddRequest {
            key: NaturalKey::MbArtist(Uuid::nil()),
            quality_profile_id: 2,
            root_folder_path: "/music".into(),
            scope: MonitorScope::Units(BTreeSet::new()),
            search: true,
            minimum_availability: None,
            metadata_profile_id: Some(5),
        };
        let metadata: Map<String, Value> = serde_json::from_value(json!({
            "artistName": "Radiohead",
            "foreignArtistId": "a74b1b7f-71a5-4011-9441-d0b5e4122711",
            "monitored": false,
            "monitorNewItems": "all",
            "qualityProfileId": 0,
            "metadataProfileId": 0
        }))
        .unwrap();
        let rg = Uuid::from_u128(7);
        let artist = new_artist(metadata, &request, 5, &[rg].into());
        assert_eq!(artist["monitored"], json!(true));
        assert_eq!(artist["monitorNewItems"], json!("none"));
        assert_eq!(artist["qualityProfileId"], json!(2));
        assert_eq!(artist["metadataProfileId"], json!(5));
        assert_eq!(artist["rootFolderPath"], json!("/music"));
        assert_eq!(
            artist["addOptions"],
            json!({
                "monitor": "none",
                "albumsToMonitor": [rg.to_string()],
                "searchForMissingAlbums": false
            })
        );
        assert_eq!(artist["artistName"], json!("Radiohead"));
    }

    #[test]
    fn scopes_must_name_albums() {
        assert!(album_scope(&MonitorScope::Item).is_err());
        assert!(album_scope(&MonitorScope::Units(BTreeSet::new())).is_err());
        assert!(album_scope(&MonitorScope::seasons([1])).is_err());
        let rg = Uuid::from_u128(7);
        assert_eq!(
            album_scope(&MonitorScope::Units([UnitId::ReleaseGroup(rg)].into())).unwrap(),
            [rg].into()
        );
    }
}
