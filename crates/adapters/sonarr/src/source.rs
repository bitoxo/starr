//! [`Source`] for Sonarr: series keyed by TVDB ID, monitored per season.
//!
//! Endpoints and their evidence: `docs/concept/capabilities.md`, section
//! "Radarr 6, Sonarr 4, Whisparr v3" (source read at `v4.0.20.3014`).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use starr_adapter_kit::servarr::{Folder, Profile, QueueRecord};
use starr_core::adapters::{AdapterError, Listing, Probe};
use starr_core::source::{
    AddOutcome, AddRequest, Files, LibraryItem, Lookup, MonitorScope, NaturalKey, Progress,
    QualityProfile, RootFolder, Source, UnitId, UnitMonitoring, UnitProgress, WriteReport,
    check_added, check_extended, check_stopped,
};

use crate::Sonarr;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Season {
    season_number: u32,
    monitored: bool,
}

/// The part of a `SeriesResource` Starr reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Series {
    id: i64,
    tvdb_id: u32,
    title: String,
    monitored: bool,
    quality_profile_id: i64,
    #[serde(default)]
    root_folder_path: Option<String>,
    #[serde(default)]
    seasons: Vec<Season>,
}

impl Series {
    fn into_item(self) -> LibraryItem {
        LibraryItem {
            remote_id: self.id,
            key: NaturalKey::TvdbSeries(self.tvdb_id),
            title: self.title,
            monitored: self.monitored,
            quality_profile_id: self.quality_profile_id,
            root_folder_path: self.root_folder_path.filter(|p| !p.is_empty()),
            units: self
                .seasons
                .into_iter()
                .map(|s| UnitMonitoring {
                    unit: UnitId::Season(s.season_number),
                    monitored: s.monitored,
                })
                .collect(),
        }
    }
}

/// The part of an `EpisodeResource` Starr reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Episode {
    season_number: u32,
    #[serde(default)]
    air_date_utc: Option<String>,
    #[serde(default)]
    has_file: bool,
}

impl Episode {
    /// An air date that does not parse counts as aired: Starr then waits
    /// for its file rather than calling the season complete too early.
    /// Aired episodes count whether monitored or not: a season is complete
    /// only when every aired episode has a file (D15).
    fn aired(&self, now: DateTime<Utc>) -> bool {
        self.air_date_utc
            .as_deref()
            .is_some_and(|date| DateTime::parse_from_rfc3339(date).map_or(true, |d| d <= now))
    }
}

/// Count files per season from the full episode list. Seasons without
/// episodes still appear (with zeros) when the series lists them.
fn season_progress(
    item: &LibraryItem,
    episodes: &[Episode],
    now: DateTime<Utc>,
) -> Vec<UnitProgress> {
    let empty = |number, monitored| UnitProgress {
        unit: UnitId::Season(number),
        monitored,
        expected: 0,
        expected_with_file: 0,
        with_file: 0,
    };
    let mut seasons: BTreeMap<u32, UnitProgress> = item
        .units
        .iter()
        .filter_map(|u| Some((u.unit.season()?, u.monitored)))
        .map(|(number, monitored)| (number, empty(number, monitored)))
        .collect();
    for episode in episodes {
        let season = seasons
            .entry(episode.season_number)
            .or_insert_with(|| empty(episode.season_number, false));
        let aired = episode.aired(now);
        season.expected += u32::from(aired);
        season.with_file += u32::from(episode.has_file);
        season.expected_with_file += u32::from(aired && episode.has_file);
    }
    seasons.into_values().collect()
}

fn tvdb_id(key: NaturalKey) -> Result<u32, AdapterError> {
    match key {
        NaturalKey::TvdbSeries(id) => Ok(id),
        NaturalKey::TmdbMovie(_) | NaturalKey::MbArtist(_) | NaturalKey::StashScene(_) => Err(
            AdapterError::Rejected(format!("Sonarr keeps series by TVDB ID, not {key}")),
        ),
    }
}

fn season_scope(scope: &MonitorScope) -> Result<BTreeSet<u32>, AdapterError> {
    let rejected = || {
        AdapterError::Rejected(
            "Sonarr series are monitored per season; name the seasons".to_owned(),
        )
    };
    match scope {
        MonitorScope::Units(units) if !units.is_empty() => units
            .iter()
            .map(|unit| unit.season().ok_or_else(rejected))
            .collect(),
        MonitorScope::Units(_) | MonitorScope::Item => Err(rejected()),
    }
}

/// Set `monitored` to `to` on the seasons of a series resource that
/// `select` names and that are not that way already. Returns the season
/// numbers changed.
fn set_seasons(
    resource: &mut Map<String, Value>,
    to: bool,
    select: impl Fn(u32) -> bool,
) -> Vec<u32> {
    let mut changed = Vec::new();
    let Some(seasons) = resource.get_mut("seasons").and_then(Value::as_array_mut) else {
        return changed;
    };
    for season in seasons.iter_mut().filter_map(Value::as_object_mut) {
        let number = season
            .get("seasonNumber")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        let monitored = season.get("monitored").and_then(Value::as_bool) == Some(true);
        if let Some(number) = number
            && monitored != to
            && select(number)
        {
            season.insert("monitored".into(), json!(to));
            changed.push(number);
        }
    }
    changed
}

/// Turn on the seasons `turn_on` names; never turns a season off.
fn monitor_seasons(resource: &mut Map<String, Value>, turn_on: impl Fn(u32) -> bool) -> Vec<u32> {
    set_seasons(resource, true, turn_on)
}

impl Sonarr {
    /// The library entry for `tvdb`, from `GET /api/v3/series?tvdbId=`.
    async fn find(&self, tvdb: u32) -> Result<Option<Series>, AdapterError> {
        let series: Vec<Series> = self
            .client
            .get_json(&format!("api/v3/series?tvdbId={tvdb}"))
            .await?;
        let mut matching = series.into_iter().filter(|s| s.tvdb_id == tvdb);
        let first = matching.next();
        if matching.next().is_some() {
            return Err(AdapterError::Unknown(format!(
                "Sonarr lists more than one series with TVDB ID {tvdb}"
            )));
        }
        Ok(first)
    }

    async fn find_required(&self, tvdb: u32) -> Result<Series, AdapterError> {
        self.find(tvdb)
            .await?
            .ok_or_else(|| AdapterError::NotFound(format!("tvdb:{tvdb} is not in Sonarr")))
    }

    async fn read(&self, id: i64) -> Result<Series, AdapterError> {
        self.client.get_json(&format!("api/v3/series/{id}")).await
    }

    async fn search_season(&self, id: i64, season: u32) -> Result<(), AdapterError> {
        let _: Value = self
            .client
            .post_json(
                "api/v3/command",
                &json!({ "name": "SeasonSearch", "seriesId": id, "seasonNumber": season }),
            )
            .await?;
        Ok(())
    }
}

impl Source for Sonarr {
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
        let tvdb = tvdb_id(key)?;
        Ok(match self.find(tvdb).await? {
            Some(series) => Lookup::InLibrary(series.into_item()),
            None => Lookup::NotInLibrary,
        })
    }

    async fn add(&self, request: &AddRequest) -> Result<AddOutcome, AdapterError> {
        let tvdb = tvdb_id(request.key)?;
        let wanted = season_scope(&request.scope)?;
        // Writes only inside the tested version range (lesson 7).
        self.probe().await?;
        // Two adds of one series within milliseconds create it twice
        // (capabilities.md): one add per TVDB ID at a time.
        let _serial = self.client.serialize(&request.key.to_string()).await;
        if let Some(existing) = self.find(tvdb).await? {
            return Ok(AddOutcome::AlreadyExists(existing.into_item()));
        }

        // Start from Sonarr's own metadata, as its UI does.
        let results: Vec<Map<String, Value>> = self
            .client
            .get_json(&format!("api/v3/series/lookup?term=tvdb%3A{tvdb}"))
            .await?;
        let mut body = results
            .into_iter()
            .find(|r| r.get("tvdbId").and_then(Value::as_u64) == Some(u64::from(tvdb)))
            .ok_or_else(|| {
                AdapterError::NotFound(format!("Sonarr's metadata does not know tvdb:{tvdb}"))
            })?;
        if let Some(seasons) = body.get_mut("seasons").and_then(Value::as_array_mut) {
            for season in seasons.iter_mut().filter_map(Value::as_object_mut) {
                let number = season
                    .get("seasonNumber")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok());
                let monitored = number.is_some_and(|n| wanted.contains(&n));
                season.insert("monitored".into(), json!(monitored));
            }
        }
        body.insert("qualityProfileId".into(), json!(request.quality_profile_id));
        body.insert("rootFolderPath".into(), json!(request.root_folder_path));
        body.insert("monitored".into(), json!(true));
        body.insert("seasonFolder".into(), json!(true));
        // New seasons are Starr's business (follows, D8): without this
        // Sonarr defaults to `all` and grabs seasons nobody requested.
        body.insert("monitorNewItems".into(), json!("none"));
        body.insert(
            "addOptions".into(),
            json!({
                // Keep the seasons' monitored flags exactly as sent.
                "monitor": "skip",
                "searchForMissingEpisodes": request.search,
                "searchForCutoffUnmetEpisodes": false,
            }),
        );

        let created: Series = match self.client.post_json("api/v3/series", &body).await {
            Ok(created) => created,
            // "already added" (400) or a lost race (409 from a constraint):
            // whatever the reason, if the series is there now it exists.
            Err(AdapterError::Rejected(reason)) => {
                return match self.find(tvdb).await? {
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
        let tvdb = tvdb_id(key)?;
        let wanted = season_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&key.to_string()).await;
        let found = self.find_required(tvdb).await?;
        let id = found.id;
        let before = found.into_item();

        // Read–modify–write of the full resource: only `monitored` flags
        // go from false to true; profile, folder and the rest stay as read.
        let path = format!("api/v3/series/{id}");
        let mut resource: Map<String, Value> = self.client.get_json(&path).await?;
        let mut written = false;
        if resource.get("monitored").and_then(Value::as_bool) != Some(true) {
            resource.insert("monitored".into(), json!(true));
            written = true;
        }
        written |= !monitor_seasons(&mut resource, |n| wanted.contains(&n)).is_empty();

        let (after, deviations) = if written {
            // `PUT /series/{id}` monitors every episode of a season whose
            // flag changed (`SeriesService.UpdateSeries`).
            self.client.put_json(&path, &resource).await?;
            let after = self.read(id).await?.into_item();
            let deviations = check_extended(&before, scope, &after);
            (after, deviations)
        } else {
            // Still report requested seasons the series does not have.
            let deviations = check_extended(&before, scope, &before);
            (before, deviations)
        };
        // Search the whole requested scope, also when nothing had to be
        // written: a retry after a failed search must not lose it, and
        // seasons that were monitored while the series was not need one too.
        if search && after.monitored {
            for &season in wanted
                .iter()
                .filter(|&&n| after.unit_monitored(UnitId::Season(n)))
            {
                self.search_season(id, season).await?;
            }
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
        let tvdb = tvdb_id(key)?;
        let stopped = season_scope(scope)?;
        self.probe().await?;
        let _serial = self.client.serialize(&key.to_string()).await;
        let found = self.find_required(tvdb).await?;
        let id = found.id;
        let before = found.into_item();
        if !stopped
            .iter()
            .any(|&n| before.unit_monitored(UnitId::Season(n)))
        {
            return Ok(WriteReport {
                item: before,
                written: false,
                deviations: Vec::new(),
            });
        }
        // Read–modify–write of the full resource: only the named seasons'
        // `monitored` flags go from true to false; the series, the other
        // seasons, profile, folder, files and queue stay as read.
        let path = format!("api/v3/series/{id}");
        let mut resource: Map<String, Value> = self.client.get_json(&path).await?;
        set_seasons(&mut resource, false, |n| stopped.contains(&n));
        // `PUT /series/{id}` unmonitors every episode of a season whose flag
        // changed (`SeriesService.UpdateSeries`). That is exactly what Starr
        // turned on only because the season was off before Starr monitored it
        // (the caller stops nothing else): episodes of it someone monitored
        // one by one before go off too.
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
        let series = self.find_required(tvdb_id(key)?).await?;
        let id = series.id;
        let item = series.into_item();
        let episodes: Vec<Episode> = self
            .client
            .get_json(&format!("api/v3/episode?seriesId={id}"))
            .await?;
        let records: Vec<QueueRecord> = self
            .client
            .get_json(&format!("api/v3/queue/details?seriesId={id}"))
            .await?;
        let seasons = season_progress(&item, &episodes, Utc::now());
        Ok(Progress {
            item,
            // Both lists are unpaged and filtered server-side by series.
            files: Files::Units(Listing {
                items: seasons,
                complete: true,
            }),
            queue: Listing {
                items: records.into_iter().map(Into::into).collect(),
                complete: true,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unparseable_air_dates_count_as_aired_missing_ones_do_not() {
        let now = Utc::now();
        let ep = |date: Option<&str>| Episode {
            season_number: 1,
            air_date_utc: date.map(str::to_owned),
            has_file: false,
        };
        assert!(ep(Some("2008-01-21T02:00:00Z")).aired(now));
        assert!(!ep(Some("2099-01-01T00:00:00Z")).aired(now));
        assert!(ep(Some("garbage")).aired(now));
        assert!(!ep(None).aired(now));
    }

    #[test]
    fn stopping_seasons_only_turns_the_named_flags_off() {
        let mut resource: Map<String, Value> = serde_json::from_value(json!({
            "monitored": true,
            "seasons": [
                { "seasonNumber": 1, "monitored": true },
                { "seasonNumber": 2, "monitored": true },
                { "seasonNumber": 3, "monitored": false },
            ]
        }))
        .unwrap();
        let changed = set_seasons(&mut resource, false, |n| n == 2 || n == 3);
        assert_eq!(changed, vec![2]);
        assert_eq!(
            resource,
            serde_json::from_value::<Map<String, Value>>(json!({
                "monitored": true,
                "seasons": [
                    { "seasonNumber": 1, "monitored": true },
                    { "seasonNumber": 2, "monitored": false },
                    { "seasonNumber": 3, "monitored": false },
                ]
            }))
            .unwrap()
        );
    }

    #[test]
    fn monitoring_seasons_only_turns_flags_on() {
        let mut resource: Map<String, Value> = serde_json::from_value(json!({
            "seasons": [
                { "seasonNumber": 1, "monitored": true },
                { "seasonNumber": 2, "monitored": false },
                { "seasonNumber": 3, "monitored": false },
            ]
        }))
        .unwrap();
        let changed = monitor_seasons(&mut resource, |n| n == 1 || n == 2);
        assert_eq!(changed, vec![2]);
        assert_eq!(
            resource["seasons"],
            json!([
                { "seasonNumber": 1, "monitored": true },
                { "seasonNumber": 2, "monitored": true },
                { "seasonNumber": 3, "monitored": false },
            ])
        );
    }
}
