//! [`Source`] for Radarr: movies keyed by TMDB ID.
//!
//! Endpoints and their evidence: `docs/concept/capabilities.md`, section
//! "Radarr 6, Sonarr 4, Whisparr v3" (source read at `v6.4.4.10685`).

use serde::Deserialize;
use serde_json::{Map, Value, json};
use starr_adapter_kit::servarr::{Folder, Profile, QueueRecord};
use starr_core::adapters::{AdapterError, Listing, Probe};
use starr_core::source::{
    AddOutcome, AddRequest, Files, LibraryItem, Lookup, MonitorScope, NaturalKey, Progress,
    QualityProfile, RootFolder, Source, WriteReport, check_added, check_extended, check_stopped,
};

use crate::Radarr;

/// The part of a `MovieResource` Starr reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Movie {
    id: i64,
    tmdb_id: u32,
    title: String,
    monitored: bool,
    quality_profile_id: i64,
    #[serde(default)]
    root_folder_path: Option<String>,
    #[serde(default)]
    has_file: Option<bool>,
}

impl Movie {
    fn into_item(self) -> LibraryItem {
        LibraryItem {
            remote_id: self.id,
            key: NaturalKey::TmdbMovie(self.tmdb_id),
            title: self.title,
            monitored: self.monitored,
            quality_profile_id: self.quality_profile_id,
            root_folder_path: self.root_folder_path.filter(|p| !p.is_empty()),
            units: Vec::new(),
        }
    }
}

fn tmdb_id(key: NaturalKey) -> Result<u32, AdapterError> {
    match key {
        NaturalKey::TmdbMovie(id) => Ok(id),
        NaturalKey::TvdbSeries(_) | NaturalKey::MbArtist(_) | NaturalKey::StashScene(_) => Err(
            AdapterError::Rejected(format!("Radarr keeps movies by TMDB ID, not {key}")),
        ),
    }
}

fn movie_scope(scope: &MonitorScope) -> Result<(), AdapterError> {
    match scope {
        MonitorScope::Item => Ok(()),
        MonitorScope::Units(_) => Err(AdapterError::Rejected(
            "Radarr movies have no seasons".to_owned(),
        )),
    }
}

impl Radarr {
    /// The library entry for `tmdb`, from `GET /api/v3/movie?tmdbId=`.
    async fn find(&self, tmdb: u32) -> Result<Option<Movie>, AdapterError> {
        let movies: Vec<Movie> = self
            .client
            .get_json(&format!("api/v3/movie?tmdbId={tmdb}"))
            .await?;
        let mut matching = movies.into_iter().filter(|m| m.tmdb_id == tmdb);
        let first = matching.next();
        if matching.next().is_some() {
            return Err(AdapterError::Unknown(format!(
                "Radarr lists more than one movie with TMDB ID {tmdb}"
            )));
        }
        Ok(first)
    }

    async fn find_required(&self, tmdb: u32) -> Result<Movie, AdapterError> {
        self.find(tmdb)
            .await?
            .ok_or_else(|| AdapterError::NotFound(format!("tmdb:{tmdb} is not in Radarr")))
    }

    async fn read(&self, id: i64) -> Result<Movie, AdapterError> {
        self.client.get_json(&format!("api/v3/movie/{id}")).await
    }
}

impl Source for Radarr {
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
        let tmdb = tmdb_id(key)?;
        Ok(match self.find(tmdb).await? {
            Some(movie) => Lookup::InLibrary(movie.into_item()),
            None => Lookup::NotInLibrary,
        })
    }

    async fn add(&self, request: &AddRequest) -> Result<AddOutcome, AdapterError> {
        let tmdb = tmdb_id(request.key)?;
        movie_scope(&request.scope)?;
        // Writes only inside the tested version range (lesson 7).
        self.probe().await?;
        let _serial = self.client.serialize(&request.key.to_string()).await;
        if let Some(existing) = self.find(tmdb).await? {
            return Ok(AddOutcome::AlreadyExists(existing.into_item()));
        }

        // Start from Radarr's own metadata, as its UI does.
        let mut body: Map<String, Value> = self
            .client
            .get_json(&format!("api/v3/movie/lookup/tmdb?tmdbId={tmdb}"))
            .await?;
        if body.get("tmdbId").and_then(Value::as_u64) != Some(u64::from(tmdb)) {
            return Err(AdapterError::Unknown(format!(
                "Radarr's lookup for tmdb:{tmdb} returned another movie"
            )));
        }
        body.insert("qualityProfileId".into(), json!(request.quality_profile_id));
        body.insert("rootFolderPath".into(), json!(request.root_folder_path));
        body.insert("monitored".into(), json!(true));
        // The category's setting (D9); by default search once the movie
        // is released, not on announcement.
        let minimum = request.minimum_availability.unwrap_or_default();
        body.insert("minimumAvailability".into(), json!(minimum.as_str()));
        body.insert(
            "addOptions".into(),
            json!({ "monitor": "movieOnly", "searchForMovie": request.search }),
        );

        let created: Movie = match self.client.post_json("api/v3/movie", &body).await {
            Ok(created) => created,
            // "already added" (400) or a lost race (409 from a constraint):
            // whatever the reason, if the movie is there now it exists.
            Err(AdapterError::Rejected(reason)) => {
                return match self.find(tmdb).await? {
                    Some(existing) => Ok(AddOutcome::AlreadyExists(existing.into_item())),
                    None => Err(AdapterError::Rejected(reason)),
                };
            }
            Err(err) => return Err(err),
        };
        // The reply is not the result (lesson 9): read it back.
        let item = self.read(created.id).await?.into_item();
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
        let tmdb = tmdb_id(key)?;
        movie_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&key.to_string()).await;
        let found = self.find_required(tmdb).await?;
        let id = found.id;
        let before = found.into_item();
        let (after, written, deviations) = if before.monitored {
            (before, false, Vec::new())
        } else {
            // Read–modify–write of the full resource: only `monitored` changes.
            let path = format!("api/v3/movie/{id}");
            let mut resource: Map<String, Value> = self.client.get_json(&path).await?;
            resource.insert("monitored".into(), json!(true));
            self.client.put_json(&path, &resource).await?;
            let after = self.read(id).await?.into_item();
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
                    &json!({ "name": "MoviesSearch", "movieIds": [id] }),
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
        let tmdb = tmdb_id(key)?;
        movie_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&key.to_string()).await;
        let found = self.find_required(tmdb).await?;
        let id = found.id;
        let before = found.into_item();
        if !before.monitored {
            return Ok(WriteReport {
                item: before,
                written: false,
                deviations: Vec::new(),
            });
        }
        // Read–modify–write of the full resource: only `monitored` changes;
        // the file, the queue and everything else stay as they are.
        let path = format!("api/v3/movie/{id}");
        let mut resource: Map<String, Value> = self.client.get_json(&path).await?;
        resource.insert("monitored".into(), json!(false));
        self.client.put_json(&path, &resource).await?;
        let after = self.read(id).await?.into_item();
        let deviations = check_stopped(&before, scope, &after);
        Ok(WriteReport {
            item: after,
            written: true,
            deviations,
        })
    }

    async fn progress(&self, key: NaturalKey) -> Result<Progress, AdapterError> {
        let movie = self.find_required(tmdb_id(key)?).await?;
        let records: Vec<QueueRecord> = self
            .client
            .get_json(&format!("api/v3/queue/details?movieId={}", movie.id))
            .await?;
        let has_file = movie.has_file.unwrap_or(false);
        Ok(Progress {
            item: movie.into_item(),
            files: Files::Movie { has_file },
            queue: Listing {
                items: records.into_iter().map(Into::into).collect(),
                // Unpaged and filtered server-side: the movie's whole queue.
                complete: true,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_movie_keys_and_whole_item_scopes_are_radarrs() {
        assert_eq!(tmdb_id(NaturalKey::TmdbMovie(603)).ok(), Some(603));
        assert!(matches!(
            tmdb_id(NaturalKey::TvdbSeries(81189)),
            Err(AdapterError::Rejected(_))
        ));
        assert!(movie_scope(&MonitorScope::Item).is_ok());
        assert!(matches!(
            movie_scope(&MonitorScope::seasons([1])),
            Err(AdapterError::Rejected(_))
        ));
    }
}
