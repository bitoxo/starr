//! What Jellyfin and Emby share (both descend from Media Browser): response
//! shapes, library item paging with completeness evidence, and the mapping
//! to [`MediaItem`]s. The adapters add their auth, paths, product check and
//! links. Evidence: `docs/concept/capabilities.md`, sections "Jellyfin" and
//! "Emby".

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use starr_core::accounts::{LibraryGrant, RemoteAccount};
use starr_core::adapters::{AdapterError, Listing};
use starr_core::library::{
    ItemKind, LibraryContent, MediaItem, MediaLibrary, ProviderIds, SeasonPresence, UnitCounts,
    encode_component,
};

use crate::ServiceClient;

pub mod writes;

/// Items per request. Servers may return fewer; paging goes by what came.
pub const PAGE_SIZE: usize = 200;
/// Upper bound on requests for one listing, against a server that keeps
/// returning pages without ever reaching its total.
const MAX_PAGES: usize = 10_000;

/// IDs are JSON strings in both services; accept numbers too (Emby's
/// numeric internal IDs) so a shape change does not break parsing.
fn id_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected an ID, found {other}"
        ))),
    }
}

fn opt_id_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected an ID, found {other}"
        ))),
    }
}

/// `GET /System/Info` (only what Starr reads).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfo {
    /// Jellyfin: `Jellyfin Server`. Emby has no such field.
    #[serde(default)]
    pub product_name: Option<String>,
    pub version: String,
    #[serde(deserialize_with = "id_string")]
    pub id: String,
}

/// The part of a user's policy Starr reads. Fields are optional so a shape
/// change is noticed: an account whose rights cannot be read is left out of
/// account listings (never reported as unprivileged).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct UserPolicy {
    is_administrator: Option<bool>,
    is_disabled: Option<bool>,
    enable_all_folders: Option<bool>,
    enabled_folders: Option<Vec<String>>,
    /// Jellyfin: failed logins counted towards the lockout.
    invalid_login_attempt_count: Option<i64>,
    /// Jellyfin: `-1` no lockout, `0` the default of 3, `n` after n.
    login_attempts_before_lockout: Option<i64>,
}

/// An account as `GET /Users` lists it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    #[serde(deserialize_with = "id_string")]
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    policy: Option<UserPolicy>,
}

/// `GET /Users` answers a plain array (Jellyfin, Emby) or a query result
/// (`/Users/Query` in Emby).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Users {
    List(Vec<User>),
    Query {
        #[serde(rename = "Items")]
        items: Vec<User>,
        #[serde(rename = "TotalRecordCount", default)]
        total: Option<u64>,
    },
}

/// The account whose view the item queries use: an enabled administrator
/// that sees every library, so a listing can be complete. Jellyfin returns
/// no items to an API key without a user (`capabilities.md`).
pub fn library_user(users: Users) -> Result<String, AdapterError> {
    let users = match users {
        Users::List(users) | Users::Query { items: users, .. } => users,
    };
    users
        .into_iter()
        .find(|u| {
            u.policy.as_ref().is_some_and(|p| {
                p.is_administrator == Some(true)
                    && p.is_disabled == Some(false)
                    && p.enable_all_folders == Some(true)
            })
        })
        .map(|u| u.id)
        .ok_or_else(|| {
            AdapterError::Rejected(
                "no enabled administrator with access to all libraries; Starr reads libraries through such an account"
                    .to_owned(),
            )
        })
}

/// How a service reports a lockout after failed logins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lockout {
    /// Jellyfin: the lockout sets `IsDisabled`; locked = disabled and the
    /// failed-login counter reached the limit (`capabilities.md`, Jellyfin).
    DisablesAccount,
    /// Emby: `LockedOutDate` records when a lock started and stays set after
    /// it expired (about a minute later), so the current lock state is not
    /// readable; reported as not locked.
    NotReadable,
}

/// Jellyfin's default limit when `LoginAttemptsBeforeLockout` is 0.
const DEFAULT_LOCKOUT_ATTEMPTS: i64 = 3;

/// One user as a [`RemoteAccount`]; `None` if its ID, name or rights cannot
/// be read.
fn remote_account(user: User, lockout: Lockout) -> Option<RemoteAccount> {
    let policy = user.policy?;
    let name = user
        .name
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())?;
    if user.id.is_empty() {
        return None;
    }
    let privileged = policy.is_administrator?;
    let disabled = policy.is_disabled?;
    let libraries = if policy.enable_all_folders? {
        LibraryGrant::All
    } else {
        LibraryGrant::Only(policy.enabled_folders?)
    };
    let locked_by_service = match lockout {
        Lockout::NotReadable => false,
        Lockout::DisablesAccount => {
            let limit = match policy.login_attempts_before_lockout.unwrap_or(-1) {
                0 => DEFAULT_LOCKOUT_ATTEMPTS,
                n => n,
            };
            disabled && limit > 0 && policy.invalid_login_attempt_count.unwrap_or(0) >= limit
        }
    };
    Some(RemoteAccount {
        remote_id: user.id,
        username: name,
        enabled: !disabled,
        privileged,
        locked_by_service,
        libraries,
    })
}

/// Accounts from `GET /Users`. The answer is not paged, so it is complete
/// unless an entry cannot be read, an ID repeats, a query-style answer
/// reports a different total, or no administrator is listed.
#[must_use]
pub fn accounts(users: Users, lockout: Lockout) -> Listing<RemoteAccount> {
    let (users, total) = match users {
        Users::List(users) => (users, None),
        Users::Query { items, total } => (items, total),
    };
    let mut complete = total.is_none_or(|t| t == users.len() as u64);
    let mut items: Vec<RemoteAccount> = Vec::with_capacity(users.len());
    let mut repeated = HashSet::new();
    for user in users {
        match remote_account(user, lockout) {
            Some(account) if items.iter().any(|a| a.remote_id == account.remote_id) => {
                repeated.insert(account.remote_id);
            }
            Some(account) => items.push(account),
            None => complete = false,
        }
    }
    // A repeated ID is ambiguous (which entry's rights hold?): leave all its
    // entries out rather than risk reporting an administrator as
    // unprivileged.
    if !repeated.is_empty() {
        complete = false;
        items.retain(|a| !repeated.contains(&a.remote_id));
    }
    // Both services always have an administrator; a listing without one
    // (empty, or cut by a proxy or a narrowed key) proves nothing.
    if !items.iter().any(|a| a.privileged) {
        complete = false;
    }
    Listing { items, complete }
}

/// One account from `GET /Users/{id}`. An entry Starr cannot read safely is
/// [`AdapterError::Unknown`].
pub fn account(user: User, lockout: Lockout) -> Result<RemoteAccount, AdapterError> {
    remote_account(user, lockout).ok_or_else(|| {
        AdapterError::Unknown("account without readable ID, name or policy".to_owned())
    })
}

/// `GET /Users`: every account (both services list hidden and disabled
/// ones to an API key).
pub async fn list_accounts(
    client: &ServiceClient,
    lockout: Lockout,
) -> Result<Listing<RemoteAccount>, AdapterError> {
    let users: Users = client.get_json("Users").await?;
    Ok(accounts(users, lockout))
}

/// `GET /Users/{id}`. An unknown ID is [`AdapterError::NotFound`]: Jellyfin
/// 12.x and Emby answer 404, Jellyfin 10.11 answers 400. An answer for
/// another ID is refused.
pub async fn read_account(
    client: &ServiceClient,
    remote_id: &str,
    lockout: Lockout,
) -> Result<RemoteAccount, AdapterError> {
    let path = format!("Users/{}", encode_component(remote_id));
    let user: User = client
        .get_json_not_found_on(&path, reqwest::StatusCode::BAD_REQUEST)
        .await?;
    let account = account(user, lockout)?;
    if account.remote_id != remote_id {
        return Err(AdapterError::Unknown(
            "the service answered with another account".to_owned(),
        ));
    }
    Ok(account)
}

/// The library user, looked up once per adapter (one sync run) so every
/// library of a run is read through the same account.
#[derive(Debug, Default)]
pub struct LibraryUser(Mutex<Option<String>>);

impl LibraryUser {
    pub async fn get(&self, client: &ServiceClient) -> Result<String, AdapterError> {
        if let Some(user) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            return Ok(user.clone());
        }
        let users: Users = client.get_json("Users").await?;
        let user = library_user(users)?;
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(user.clone());
        Ok(user)
    }
}

/// `GET /Library/VirtualFolders` item.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct VirtualFolder {
    name: String,
    /// The library's item ID, used as `ParentId` in item queries.
    #[serde(default, deserialize_with = "opt_id_string")]
    item_id: Option<String>,
    /// Emby: the ID `EnabledFolders` grants take (`capabilities.md`).
    #[serde(default)]
    guid: Option<String>,
    #[serde(default)]
    collection_type: Option<String>,
}

/// Which field of a virtual folder account grants name the library by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKey {
    /// Jellyfin: `EnabledFolders` takes the `ItemId`.
    ItemId,
    /// Emby: `EnabledFolders` takes the `Guid`.
    Guid,
}

fn content(collection_type: Option<&str>) -> LibraryContent {
    match collection_type.map(str::to_ascii_lowercase).as_deref() {
        Some("movies") => LibraryContent::Movies,
        Some("tvshows") => LibraryContent::Series,
        Some("music") => LibraryContent::Music,
        Some("books" | "audiobooks") => LibraryContent::Books,
        _ => LibraryContent::Other,
    }
}

/// Libraries from `GET /Library/VirtualFolders`. A folder without an item
/// ID cannot be listed: it is left out and the listing is then incomplete.
/// One without its grant ID is listed with `grant_id: None` (the inventory
/// still reads it; it just cannot be granted).
#[must_use]
pub fn libraries(folders: Vec<VirtualFolder>, grant: GrantKey) -> Listing<MediaLibrary> {
    let mut complete = true;
    let items = folders
        .into_iter()
        .filter_map(|f| {
            let Some(id) = f.item_id.filter(|id| !id.is_empty()) else {
                complete = false;
                return None;
            };
            let grant_id = match grant {
                GrantKey::ItemId => Some(id.clone()),
                GrantKey::Guid => f.guid.filter(|g| !g.trim().is_empty()),
            };
            Some(MediaLibrary {
                id,
                grant_id,
                content: content(f.collection_type.as_deref()),
                name: f.name,
            })
        })
        .collect();
    Listing { items, complete }
}

/// A library item as the item queries return it (only what Starr reads).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Item {
    #[serde(deserialize_with = "id_string")]
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "Type", default)]
    kind: Option<String>,
    #[serde(default)]
    production_year: Option<u32>,
    #[serde(default)]
    provider_ids: Option<HashMap<String, Value>>,
    #[serde(default, deserialize_with = "opt_id_string")]
    series_id: Option<String>,
    #[serde(default)]
    parent_index_number: Option<u32>,
    #[serde(default)]
    index_number: Option<u32>,
    #[serde(default)]
    index_number_end: Option<u32>,
    #[serde(default)]
    location_type: Option<String>,
    /// Music albums: their tracks (`Fields=ChildCount`).
    #[serde(default)]
    child_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemsPage {
    #[serde(default)]
    items: Vec<Item>,
    #[serde(default)]
    total_record_count: Option<u64>,
}

/// What kind of items an item query asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemQuery {
    MoviesAndSeries,
    Episodes,
    /// Music albums with their provider IDs and track counts (spec 0006
    /// Q7 B).
    Albums,
}

fn query_params(
    library_id: &str,
    what: ItemQuery,
    start: usize,
    extra: &[(&'static str, String)],
) -> Vec<(&'static str, String)> {
    let mut query = vec![
        ("ParentId", library_id.to_owned()),
        ("Recursive", "true".to_owned()),
        // A stable order. Items added or removed while paging shift pages
        // (`DateCreated` may come from file times); that shows as a changed
        // total or duplicate IDs (incomplete), except one addition plus one
        // removal between two pages, which the inventory's two-complete-runs
        // rule absorbs.
        ("SortBy", "DateCreated,SortName".to_owned()),
        ("SortOrder", "Ascending".to_owned()),
        ("EnableTotalRecordCount", "true".to_owned()),
        ("EnableImages", "false".to_owned()),
        ("EnableUserData", "false".to_owned()),
        ("StartIndex", start.to_string()),
        ("Limit", PAGE_SIZE.to_string()),
    ];
    match what {
        ItemQuery::MoviesAndSeries => {
            query.push(("IncludeItemTypes", "Movie,Series".to_owned()));
            query.push(("Fields", "ProviderIds".to_owned()));
        }
        ItemQuery::Episodes => {
            query.push(("IncludeItemTypes", "Episode".to_owned()));
            query.push(("IsMissing", "false".to_owned()));
        }
        ItemQuery::Albums => {
            query.push(("IncludeItemTypes", "MusicAlbum".to_owned()));
            query.push(("Fields", "ProviderIds,ChildCount".to_owned()));
        }
    }
    query.extend(extra.iter().cloned());
    query
}

/// Page through an item query at `path`. Complete only when every page
/// reported the same total and exactly that many distinct items arrived
/// (lesson 8); a missing total, a total that changes while paging, or a
/// page that ends early make the listing incomplete.
pub async fn list_items(
    client: &ServiceClient,
    path: &str,
    library_id: &str,
    what: ItemQuery,
    extra: &[(&'static str, String)],
) -> Result<Listing<Item>, AdapterError> {
    let mut items: Vec<Item> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut total: Option<u64> = None;
    let mut start = 0usize;
    for _ in 0..MAX_PAGES {
        let query = query_params(library_id, what, start, extra);
        let page: ItemsPage = client.get_json_cached(path, &query, None).await?;
        let Some(page_total) = page.total_record_count else {
            items.extend(page.items.into_iter().filter(|i| seen.insert(i.id.clone())));
            return Ok(Listing {
                items,
                complete: false,
            });
        };
        if total.is_some_and(|t| t != page_total) {
            items.extend(page.items.into_iter().filter(|i| seen.insert(i.id.clone())));
            return Ok(Listing {
                items,
                complete: false,
            });
        }
        total = Some(page_total);
        let received = page.items.len();
        start += received;
        items.extend(page.items.into_iter().filter(|i| seen.insert(i.id.clone())));
        let done = start as u64 >= page_total;
        if done || received == 0 {
            let complete = seen.len() as u64 == page_total && start as u64 == page_total;
            return Ok(Listing { items, complete });
        }
    }
    Ok(Listing {
        items,
        complete: false,
    })
}

fn provider_ids(item: &Item) -> ProviderIds {
    let pairs = item
        .provider_ids
        .iter()
        .flatten()
        .filter_map(|(k, v)| match v {
            Value::String(s) => Some((k.as_str(), s.as_str())),
            _ => None,
        });
    ProviderIds::from_pairs(pairs)
}

/// Turn the two query results into movies and series with their present
/// episodes per season. Complete only if both listings are.
#[must_use]
pub fn assemble(primary: Listing<Item>, episodes: Option<Listing<Item>>) -> Listing<MediaItem> {
    // Series ID → season → distinct episode numbers.
    let mut present: HashMap<String, BTreeMap<u32, BTreeSet<u32>>> = HashMap::new();
    let episodes_complete = episodes.as_ref().is_none_or(|l| l.complete);
    for ep in episodes.into_iter().flat_map(|l| l.items) {
        let virtual_item = ep
            .location_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("virtual"));
        let (Some(series), Some(season), Some(first)) =
            (ep.series_id, ep.parent_index_number, ep.index_number)
        else {
            continue;
        };
        if virtual_item {
            continue;
        }
        // A file holding several episodes (`IndexNumberEnd`) counts each,
        // bounded so a bogus end cannot blow up the set.
        let last = ep
            .index_number_end
            .unwrap_or(first)
            .clamp(first, first.saturating_add(50));
        present
            .entry(series)
            .or_default()
            .entry(season)
            .or_default()
            .extend(first..=last);
    }

    let complete = primary.complete && episodes_complete;
    let items = primary
        .items
        .into_iter()
        .filter_map(|item| {
            let kind = match item.kind.as_deref() {
                Some("Movie") => ItemKind::Movie,
                Some("Series") => ItemKind::Series,
                Some("MusicAlbum") => ItemKind::Album,
                _ => return None,
            };
            let units = match kind {
                ItemKind::Series => {
                    UnitCounts::Seasons(present.get(&item.id).map_or_default(|seasons| {
                        seasons
                            .iter()
                            .map(|(&number, episodes)| SeasonPresence {
                                number,
                                episodes: u32::try_from(episodes.len()).unwrap_or(u32::MAX),
                            })
                            .collect()
                    }))
                }
                // An album without a track count is listed with none: it
                // never counts as complete (positive evidence only).
                ItemKind::Album => UnitCounts::Tracks(item.child_count.unwrap_or(0)),
                _ => UnitCounts::Whole,
            };
            Some(MediaItem {
                provider_ids: provider_ids(&item),
                remote_id: item.id,
                kind,
                title: item.name.unwrap_or_default(),
                year: item.production_year,
                units,
            })
        })
        .collect();
    Listing { items, complete }
}

/// Whether a movies-and-series listing contains series (and so needs the
/// episode query).
#[must_use]
pub fn has_series(listing: &Listing<Item>) -> bool {
    listing
        .items
        .iter()
        .any(|i| i.kind.as_deref() == Some("Series"))
}

/// What each library holds, as the adapter's last library listing said:
/// a music library is read for albums, every other one for movies and
/// series (spec 0006 slice 5).
#[derive(Debug, Default)]
pub struct LibraryContents(Mutex<HashMap<String, LibraryContent>>);

impl LibraryContents {
    /// Remember the contents of a library listing.
    pub fn remember(&self, listing: &Listing<MediaLibrary>) {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        for library in &listing.items {
            map.insert(library.id.clone(), library.content);
        }
    }

    fn of(&self, library_id: &str) -> Option<LibraryContent> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(library_id)
            .copied()
    }
}

/// The items of one library at item query `path`: albums of a music
/// library, else movies and series with the episodes of the series.
/// `extra` carries the library user where the path does not.
pub async fn library_items(
    client: &ServiceClient,
    path: &str,
    library_id: &str,
    extra: &[(&'static str, String)],
    contents: &LibraryContents,
) -> Result<Listing<MediaItem>, AdapterError> {
    if contents.of(library_id) == Some(LibraryContent::Music) {
        let albums = list_items(client, path, library_id, ItemQuery::Albums, extra).await?;
        return Ok(assemble(albums, None));
    }
    let primary = list_items(client, path, library_id, ItemQuery::MoviesAndSeries, extra).await?;
    let episodes = if has_series(&primary) {
        Some(list_items(client, path, library_id, ItemQuery::Episodes, extra).await?)
    } else {
        None
    };
    Ok(assemble(primary, episodes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_albums_carry_their_release_group_and_track_count() {
        let primary = Listing {
            items: [
                r#"{"Id":"a1","Name":"OK Computer","Type":"MusicAlbum","ProductionYear":1997,"ChildCount":12,"ProviderIds":{"MusicBrainzReleaseGroup":"b1392450-e666-3926-a536-22c65f834433","MusicBrainzAlbumArtist":"a74b1b7f-71a5-4011-9441-d0b5e4122711","MusicBrainzAlbum":"x"}}"#,
                // Not tagged: listed, but without a release group.
                r#"{"Id":"a2","Name":"Untagged","Type":"MusicAlbum"}"#,
                r#"{"Id":"t1","Name":"Track","Type":"Audio"}"#,
            ]
            .into_iter()
            .map(item)
            .collect(),
            complete: true,
        };
        let listing = assemble(primary, None);
        assert!(listing.complete);
        assert_eq!(listing.items.len(), 2);
        let ok = &listing.items[0];
        assert_eq!(ok.kind, ItemKind::Album);
        assert_eq!(ok.units, UnitCounts::Tracks(12));
        assert_eq!(ok.year, Some(1997));
        assert_eq!(
            ok.provider_ids.mb_release_group.map(|u| u.to_string()),
            Some("b1392450-e666-3926-a536-22c65f834433".to_owned())
        );
        assert_eq!(listing.items[1].units, UnitCounts::Tracks(0));
        assert!(listing.items[1].provider_ids.is_empty());
    }

    fn item(json: &str) -> Item {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn users_come_as_list_or_query_result() {
        let admin = r#"{"Id":"a","Policy":{"IsAdministrator":true,"IsDisabled":false,"EnableAllFolders":true}}"#;
        let limited = r#"{"Id":"b","Policy":{"IsAdministrator":true,"IsDisabled":false,"EnableAllFolders":false}}"#;
        let disabled = r#"{"Id":"c","Policy":{"IsAdministrator":true,"IsDisabled":true,"EnableAllFolders":true}}"#;
        let list: Users = serde_json::from_str(&format!("[{limited},{disabled},{admin}]")).unwrap();
        assert_eq!(library_user(list).unwrap(), "a");
        let query: Users =
            serde_json::from_str(&format!(r#"{{"Items":[{admin}],"TotalRecordCount":1}}"#))
                .unwrap();
        assert_eq!(library_user(query).unwrap(), "a");
        let none: Users = serde_json::from_str(&format!("[{limited},{disabled}]")).unwrap();
        assert!(matches!(library_user(none), Err(AdapterError::Rejected(_))));
    }

    #[test]
    fn unreadable_or_repeated_accounts_make_the_listing_incomplete() {
        let admin = r#"{"Id":"z","Name":"root","Policy":{"IsAdministrator":true,"IsDisabled":false,"EnableAllFolders":true}}"#;
        let ok = r#"{"Id":"a","Name":"Alex","Policy":{"IsAdministrator":false,"IsDisabled":false,"EnableAllFolders":false,"EnabledFolders":["m"]}}"#;
        let no_policy = r#"{"Id":"b","Name":"Bo"}"#;
        let no_admin_flag =
            r#"{"Id":"c","Name":"Cy","Policy":{"IsDisabled":false,"EnableAllFolders":true}}"#;
        let no_name = r#"{"Id":"d","Name":" ","Policy":{"IsAdministrator":false,"IsDisabled":false,"EnableAllFolders":true}}"#;
        let no_folders = r#"{"Id":"e","Name":"Ed","Policy":{"IsAdministrator":false,"IsDisabled":false,"EnableAllFolders":false}}"#;
        let list = |entries: &[&str]| -> Listing<RemoteAccount> {
            let users: Users = serde_json::from_str(&format!("[{}]", entries.join(","))).unwrap();
            accounts(users, Lockout::NotReadable)
        };
        let listing = list(&[admin, ok]);
        assert!(listing.complete);
        assert_eq!(
            listing.items[1].libraries,
            LibraryGrant::Only(vec!["m".to_owned()])
        );
        for bad in [no_policy, no_admin_flag, no_name, no_folders] {
            let listing = list(&[admin, ok, bad]);
            assert!(!listing.complete, "{bad}");
            assert_eq!(listing.items.len(), 2, "{bad}");
        }
        let query: Users = serde_json::from_str(&format!(
            r#"{{"Items":[{admin},{ok}],"TotalRecordCount":3}}"#
        ))
        .unwrap();
        assert!(!accounts(query, Lockout::NotReadable).complete);
    }

    #[test]
    fn a_repeated_id_drops_every_entry_of_it() {
        let admin = r#"{"Id":"z","Name":"root","Policy":{"IsAdministrator":true,"IsDisabled":false,"EnableAllFolders":true}}"#;
        let plain = r#"{"Id":"a","Name":"Alex","Policy":{"IsAdministrator":false,"IsDisabled":false,"EnableAllFolders":true}}"#;
        let promoted = r#"{"Id":"a","Name":"Alex","Policy":{"IsAdministrator":true,"IsDisabled":false,"EnableAllFolders":true}}"#;
        let users: Users = serde_json::from_str(&format!("[{admin},{plain},{promoted}]")).unwrap();
        let listing = accounts(users, Lockout::NotReadable);
        assert!(!listing.complete);
        let ids: Vec<_> = listing.items.iter().map(|a| a.remote_id.as_str()).collect();
        assert_eq!(ids, ["z"], "never reported as unprivileged");
    }

    #[test]
    fn a_listing_without_an_administrator_proves_nothing() {
        let plain = r#"{"Id":"a","Name":"Alex","Policy":{"IsAdministrator":false,"IsDisabled":false,"EnableAllFolders":true}}"#;
        for body in ["[]".to_owned(), format!("[{plain}]")] {
            let users: Users = serde_json::from_str(&body).unwrap();
            assert!(!accounts(users, Lockout::NotReadable).complete, "{body}");
        }
    }

    #[test]
    fn a_jellyfin_lockout_needs_the_flag_and_the_counter() {
        let user = |disabled: bool, count: i64, limit: i64| -> User {
            serde_json::from_str(&format!(
                r#"{{"Id":"a","Name":"a","Policy":{{"IsAdministrator":false,"IsDisabled":{disabled},"EnableAllFolders":true,"InvalidLoginAttemptCount":{count},"LoginAttemptsBeforeLockout":{limit}}}}}"#
            ))
            .unwrap()
        };
        let locked = |u| {
            account(u, Lockout::DisablesAccount)
                .unwrap()
                .locked_by_service
        };
        assert!(locked(user(true, 3, 3)));
        assert!(locked(user(true, 3, 0)), "0 means the default of 3");
        assert!(!locked(user(true, 2, 3)), "disabled by an admin");
        assert!(!locked(user(true, 9, -1)), "no lockout configured");
        assert!(!locked(user(false, 3, 3)), "lifted");
        assert!(
            !account(user(true, 3, 3), Lockout::NotReadable)
                .unwrap()
                .locked_by_service
        );
    }

    #[test]
    fn folders_map_to_libraries_and_numeric_ids_are_accepted() {
        let folders: Vec<VirtualFolder> = serde_json::from_str(
            r#"[{"Name":"Movies","ItemId":"f137a2dd21bbc1b99aa5c0f6bf02a805","CollectionType":"movies"},
                {"Name":"Shows","ItemId":7,"CollectionType":"tvshows"},
                {"Name":"Mixed","ItemId":"9"}]"#,
        )
        .unwrap();
        let listing = libraries(folders, GrantKey::ItemId);
        assert!(listing.complete);
        let got: Vec<_> = listing
            .items
            .iter()
            .map(|l| (l.id.as_str(), l.content))
            .collect();
        assert_eq!(
            got,
            vec![
                ("f137a2dd21bbc1b99aa5c0f6bf02a805", LibraryContent::Movies),
                ("7", LibraryContent::Series),
                ("9", LibraryContent::Other)
            ]
        );
        let folders: Vec<VirtualFolder> =
            serde_json::from_str(r#"[{"Name":"Broken","CollectionType":"movies"}]"#).unwrap();
        assert!(!libraries(folders, GrantKey::ItemId).complete);
    }

    #[test]
    fn grant_ids_come_from_the_field_the_service_grants_by() {
        let folders = || -> Vec<VirtualFolder> {
            serde_json::from_str(
                r#"[{"Name":"Movies","ItemId":"3","Guid":"0c41907140d802bb58430fed7e2cd79e","CollectionType":"movies"},
                    {"Name":"Adult","ItemId":12,"Guid":"a2b3c4d5e6f708192a3b4c5d6e7f8091"}]"#,
            )
            .unwrap()
        };
        let ids = |listing: Listing<MediaLibrary>| -> Vec<(String, Option<String>)> {
            listing
                .items
                .into_iter()
                .map(|l| (l.id, l.grant_id))
                .collect()
        };
        assert_eq!(
            ids(libraries(folders(), GrantKey::Guid)),
            vec![
                (
                    "3".to_owned(),
                    Some("0c41907140d802bb58430fed7e2cd79e".to_owned())
                ),
                (
                    "12".to_owned(),
                    Some("a2b3c4d5e6f708192a3b4c5d6e7f8091".to_owned())
                ),
            ]
        );
        assert_eq!(
            ids(libraries(folders(), GrantKey::ItemId)),
            vec![
                ("3".to_owned(), Some("3".to_owned())),
                ("12".to_owned(), Some("12".to_owned())),
            ]
        );
        // Emby without a `Guid`: still listed (the inventory reads it), but
        // without a grant ID, never the `ItemId` instead (Emby stores
        // unknown grant IDs silently).
        let folders: Vec<VirtualFolder> =
            serde_json::from_str(r#"[{"Name":"Movies","ItemId":"3","CollectionType":"movies"}]"#)
                .unwrap();
        let listing = libraries(folders, GrantKey::Guid);
        assert!(listing.complete);
        assert_eq!(listing.items[0].grant_id, None);
    }

    #[test]
    fn episodes_are_counted_per_season_without_virtual_ones() {
        let primary = Listing {
            items: vec![item(
                r#"{"Id":"s1","Name":"Show","Type":"Series","ProviderIds":{"Tvdb":"1","Tmdb":null}}"#,
            )],
            complete: true,
        };
        let episodes = Listing {
            items: [
                r#"{"Id":"e1","Type":"Episode","SeriesId":"s1","ParentIndexNumber":1,"IndexNumber":1}"#,
                r#"{"Id":"e2","Type":"Episode","SeriesId":"s1","ParentIndexNumber":1,"IndexNumber":2,"IndexNumberEnd":3}"#,
                // A second version of episode 1 counts once.
                r#"{"Id":"e3","Type":"Episode","SeriesId":"s1","ParentIndexNumber":1,"IndexNumber":1}"#,
                r#"{"Id":"e4","Type":"Episode","SeriesId":"s1","ParentIndexNumber":2,"IndexNumber":1,"LocationType":"Virtual"}"#,
                r#"{"Id":"e5","Type":"Episode","SeriesId":"s1","ParentIndexNumber":0,"IndexNumber":1}"#,
                r#"{"Id":"e6","Type":"Episode","SeriesId":"s1","IndexNumber":9}"#,
                r#"{"Id":"e7","Type":"Episode","SeriesId":"other","ParentIndexNumber":1,"IndexNumber":1}"#,
            ]
            .into_iter()
            .map(item)
            .collect(),
            complete: false,
        };
        let listing = assemble(primary, Some(episodes));
        assert!(!listing.complete, "incomplete episodes make it incomplete");
        assert_eq!(listing.items.len(), 1);
        let series = &listing.items[0];
        assert_eq!(series.provider_ids.tvdb, Some(1));
        assert_eq!(series.provider_ids.tmdb, None);
        assert_eq!(
            series.units.seasons(),
            vec![
                SeasonPresence {
                    number: 0,
                    episodes: 1
                },
                SeasonPresence {
                    number: 1,
                    episodes: 3
                }
            ]
        );
    }

    #[test]
    fn bogus_episode_numbers_never_overflow() {
        let primary = Listing {
            items: vec![item(r#"{"Id":"s1","Type":"Series"}"#)],
            complete: true,
        };
        let episodes = Listing {
            items: [
                r#"{"Id":"e1","SeriesId":"s1","ParentIndexNumber":1,"IndexNumber":4294967295,"IndexNumberEnd":4294967295}"#,
                r#"{"Id":"e2","SeriesId":"s1","ParentIndexNumber":1,"IndexNumber":4294967290,"IndexNumberEnd":3}"#,
            ]
            .into_iter()
            .map(item)
            .collect(),
            complete: true,
        };
        let listing = assemble(primary, Some(episodes));
        assert_eq!(
            listing.items[0].units.seasons(),
            vec![SeasonPresence {
                number: 1,
                episodes: 2
            }]
        );
    }
}
