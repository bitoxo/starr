//! Navidrome adapter (ADR 0009, spec 0006 slices 5 and 7): [`Probe`] and
//! [`Library`] — Navidrome's libraries and albums, keyed by the albums'
//! `MusicBrainz` release-group IDs from the file tags; reads are `GET`s
//! besides the admin login (`POST /auth/login`). [`Accounts`]
//! (`accounts.rs`, slice 7): users, their libraries and passwords, with
//! the writes listed there.
//!
//! Navidrome has no admin API key: the admin's username and password are
//! exchanged for a JWT (`X-ND-Authorization: Bearer …`), renewed by every
//! response (sliding) and kept by the shared [`ServiceClient`], so a sync
//! logs in only when it has no token or the token was refused. Logins keep
//! to one per 20 s per instance (Navidrome allows 5 per 20 s per IP, for
//! every user behind it).
//!
//! Facts and their evidence: `docs/concept/capabilities.md`, section
//! "Navidrome". The native API is not documented as stable; the shapes
//! read here come from Navidrome's source (`model/album.go`,
//! `persistence/album_repository.go`, `server/auth.go`), not from a
//! recording yet.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use secrecy::ExposeSecret;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use starr_adapter_kit::{AuthScheme, ServiceClient};
mod accounts;

pub use accounts::ACCOUNT_RULES;

use starr_core::adapters::{
    AdapterDescriptor, AdapterError, Budgets, Capability, IdentityRule, Listing, Probe,
    ProbeReport, RateLimit, SecretClass, Version, VersionRange,
};
use starr_core::library::{
    ItemKind, ItemLink, Library, LibraryContent, MediaItem, MediaLibrary, ProviderIds, UnitCounts,
    encode_component,
};
use uuid::Uuid;

/// The session token travels in Navidrome's own header.
pub const AUTH: AuthScheme = AuthScheme::Session("X-ND-Authorization");

/// Albums per page of `GET /api/album`.
pub const PAGE_SIZE: usize = 200;
/// Upper bound on pages for one listing, against a server that never
/// reaches its total.
const MAX_PAGES: usize = 10_000;

pub const DESCRIPTOR: AdapterDescriptor = AdapterDescriptor {
    type_id: "navidrome",
    display_name: "Navidrome",
    capabilities: &[Capability::Libraries, Capability::Accounts],
    // Multi-library (`library_id`, `/api/library`) since 0.58.0; 0.64.x is
    // the newest line read (0.64.0 re-encoded internal IDs, which Starr
    // only caches).
    tested_versions: VersionRange {
        min: Version::new(0, 58, 0, 0),
        max_exclusive: Version::new(0, 65, 0, 0),
    },
    budgets: Budgets {
        rate_limit: RateLimit {
            requests: 10,
            per: Duration::from_secs(1),
        },
        // Navidrome: 5 logins per 20 s per client IP, for all users behind
        // it (`capabilities.md`). Starr takes one of them.
        login_limit: Some(RateLimit {
            requests: 1,
            per: Duration::from_secs(20),
        }),
        expensive_endpoints: &["api/album"],
    },
    identity: IdentityRule::RemoteId,
    // An admin login: opened only by the worker (ADR 0003).
    secret_class: SecretClass::ServiceCredential,
    lists: &[],
};

#[derive(Serialize)]
struct LoginBody<'a> {
    username: &'a str,
    password: &'a str,
}

/// The part of the login answer Starr reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginReply {
    token: String,
    #[serde(default)]
    is_admin: Option<bool>,
}

/// `GET /rest/ping`: every Subsonic answer, also the error for a missing
/// login, names the server type and version.
#[derive(Debug, Deserialize)]
struct Ping {
    #[serde(rename = "subsonic-response")]
    response: PingResponse,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PingResponse {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    server_version: Option<String>,
}

/// One library of `GET /api/library` (also in user objects).
#[derive(Debug, Deserialize)]
struct WireLibrary {
    id: u64,
    #[serde(default)]
    name: String,
}

/// One album of `GET /api/album` (only what Starr reads).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAlbum {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    library_id: Option<u64>,
    #[serde(default)]
    max_year: Option<u32>,
    #[serde(default)]
    song_count: Option<u32>,
    #[serde(default)]
    mbz_release_group_id: Option<String>,
    #[serde(default)]
    mbz_album_artist_id: Option<String>,
    /// Every file of the album is gone; Navidrome keeps the album until a
    /// purge.
    #[serde(default)]
    missing: bool,
}

/// The version in `serverVersion` (`0.64.2 (abc1234)`, `0.64.2-SNAPSHOT`).
fn server_version(raw: &str) -> Option<Version> {
    let numeric: String = raw
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    numeric.trim_end_matches('.').parse().ok()
}

fn mbid(value: Option<&str>) -> Option<Uuid> {
    value
        .and_then(|v| Uuid::try_parse(v.trim()).ok())
        .filter(|id| !id.is_nil())
}

fn media_item(album: WireAlbum) -> MediaItem {
    MediaItem {
        provider_ids: ProviderIds {
            mb_release_group: mbid(album.mbz_release_group_id.as_deref()),
            mb_artist: mbid(album.mbz_album_artist_id.as_deref()),
            ..ProviderIds::default()
        },
        remote_id: album.id,
        kind: ItemKind::Album,
        title: album.name,
        year: album.max_year.filter(|y| *y > 0),
        units: UnitCounts::Tracks(album.song_count.unwrap_or(0)),
    }
}

/// Send a request with the session: log in first if there is no token, and
/// once more if the service refused the one Starr had (expired, or the
/// admin's password changed). The request expression is built anew for
/// the retry.
macro_rules! session {
    ($self:ident, $request:expr) => {{
        let had_session = $self.client.has_session();
        if !had_session {
            $self.login().await?;
        }
        match $request.await {
            Err(AdapterError::CredentialsInvalid) if had_session => {
                $self.client.clear_session();
                $self.login().await?;
                $request.await
            }
            other => other,
        }
    }};
}

#[derive(Debug)]
pub struct Navidrome {
    client: ServiceClient,
}

impl Navidrome {
    #[must_use]
    pub fn new(client: ServiceClient) -> Self {
        Self { client }
    }

    /// Log in with the stored admin login and keep the token. Only an
    /// administrator sees every library.
    async fn login(&self) -> Result<(), AdapterError> {
        let Some(login) = self.client.login() else {
            return Err(AdapterError::Unknown(
                "the Navidrome client has no login".to_owned(),
            ));
        };
        self.client.login_turn()?;
        let reply: LoginReply = self
            .client
            .post_json_sensitive(
                "auth/login",
                &LoginBody {
                    username: &login.username,
                    password: login.password.expose_secret(),
                },
                true,
            )
            .await?;
        if reply.is_admin != Some(true) {
            return Err(AdapterError::Rejected(
                "the Navidrome account is no administrator; Starr reads the libraries through one"
                    .to_owned(),
            ));
        }
        self.client.set_session_token(&reply.token)
    }

    /// `GET` with the session ([`session!`]).
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(T, Option<u64>), AdapterError> {
        session!(self, self.client.get_json_counted(path, query))
    }

    /// `PUT` with the session; `sensitive`: the body carries a password,
    /// an error reply is neither logged nor quoted. A refused token means
    /// nothing was written, so the write is sent once more after a login.
    async fn put(&self, path: &str, body: &Value, sensitive: bool) -> Result<(), AdapterError> {
        session!(self, self.client.put_json_sensitive(path, body, sensitive))
    }

    /// `POST` with the session; the body always carries a password.
    async fn post(&self, path: &str, body: &Value) -> Result<Value, AdapterError> {
        session!(self, self.client.post_json_sensitive(path, body, true))
    }

    /// `DELETE` with the session.
    async fn delete(&self, path: &str) -> Result<(), AdapterError> {
        session!(self, self.client.delete(path))
    }
}

impl Probe for Navidrome {
    fn descriptor(&self) -> &'static AdapterDescriptor {
        &DESCRIPTOR
    }

    /// Product and version from the Subsonic ping (no credential sent),
    /// then the login (or, with a token, a library read) proves the
    /// credential.
    async fn probe(&self) -> Result<ProbeReport, AdapterError> {
        let ping: Ping = self
            .client
            .get_json_cached(
                "rest/ping",
                &[
                    ("v", "1.16.1".to_owned()),
                    ("c", "starr".to_owned()),
                    ("f", "json".to_owned()),
                ],
                None,
            )
            .await?;
        if !ping
            .response
            .kind
            .as_deref()
            .is_some_and(|k| k.eq_ignore_ascii_case("navidrome"))
        {
            return Err(AdapterError::Unknown(
                "the service at this URL is no Navidrome".to_owned(),
            ));
        }
        let raw = ping.response.server_version.unwrap_or_default();
        let version = server_version(&raw).ok_or_else(|| {
            AdapterError::Unknown(format!(
                "unparseable version {:?}",
                self.client.redactor().redact(&raw)
            ))
        })?;
        let _: (Vec<WireLibrary>, _) = self.get("api/library", &[]).await?;
        DESCRIPTOR.check_version(version)?;
        Ok(ProbeReport { version })
    }
}

impl Library for Navidrome {
    /// Navidrome has no server ID.
    fn server_id(&self) -> impl Future<Output = Result<Option<String>, AdapterError>> + Send {
        std::future::ready(Ok(None))
    }

    /// `GET /api/library`, unpaged: complete unless a total disagrees or an
    /// ID repeats. Every Navidrome library holds music.
    async fn libraries(&self) -> Result<Listing<MediaLibrary>, AdapterError> {
        let (libraries, total): (Vec<WireLibrary>, _) = self.get("api/library", &[]).await?;
        let mut complete = total.is_none_or(|t| t == libraries.len() as u64);
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(libraries.len());
        for library in libraries {
            if !seen.insert(library.id) {
                complete = false;
                continue;
            }
            let id = library.id.to_string();
            items.push(MediaLibrary {
                grant_id: Some(id.clone()),
                id,
                name: library.name,
                content: LibraryContent::Music,
            });
        }
        Ok(Listing { items, complete })
    }

    /// The albums of one library, paged by `X-Total-Count` (react-admin):
    /// complete only when every page reported the same total and exactly
    /// that many distinct albums of this library arrived (lesson 8).
    /// Albums whose files are all missing are counted, not listed.
    async fn items(&self, library_id: &str) -> Result<Listing<MediaItem>, AdapterError> {
        let wanted: u64 = library_id.parse().map_err(|_| {
            AdapterError::Rejected(format!("{library_id:?} is no Navidrome library ID"))
        })?;
        let mut items = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut total: Option<u64> = None;
        let mut start = 0usize;
        let mut foreign = false;
        for _ in 0..MAX_PAGES {
            let query = [
                ("_start", start.to_string()),
                ("_end", (start + PAGE_SIZE).to_string()),
                ("_sort", "id".to_owned()),
                ("_order", "ASC".to_owned()),
                ("library_id", library_id.to_owned()),
            ];
            let (page, page_total): (Vec<WireAlbum>, _) = self.get("api/album", &query).await?;
            let received = page.len();
            for album in page {
                // An album of another library: the filter was not applied
                // (a Navidrome without it); never file it under this one.
                if album.library_id.is_some_and(|l| l != wanted) {
                    foreign = true;
                    continue;
                }
                if !seen.insert(album.id.clone()) || album.missing {
                    continue;
                }
                items.push(media_item(album));
            }
            let Some(page_total) = page_total else {
                return Ok(Listing {
                    items,
                    complete: false,
                });
            };
            if total.is_some_and(|t| t != page_total) {
                return Ok(Listing {
                    items,
                    complete: false,
                });
            }
            total = Some(page_total);
            start += received;
            if start as u64 >= page_total || received == 0 {
                let complete =
                    !foreign && start as u64 == page_total && seen.len() as u64 == page_total;
                return Ok(Listing { items, complete });
            }
        }
        Ok(Listing {
            items,
            complete: false,
        })
    }

    /// `…/app/#/album/<id>/show`, the album page of Navidrome's web UI.
    fn item_url(link: &ItemLink<'_>) -> String {
        format!(
            "{}/app/#/album/{}/show",
            link.public_url.trim_end_matches('/'),
            encode_component(link.remote_id)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_the_server_version() {
        assert_eq!(
            server_version("0.64.2 (6f3a1b2)"),
            Some(Version::new(0, 64, 2, 0))
        );
        assert_eq!(
            server_version("0.58.0-SNAPSHOT"),
            Some(Version::new(0, 58, 0, 0))
        );
        assert_eq!(server_version("dev"), None);
        assert_eq!(server_version(""), None);
    }

    #[test]
    fn links_open_the_album_page() {
        let link = ItemLink {
            public_url: "https://music.example.com/",
            server_id: None,
            remote_id: "3Kp a",
        };
        assert_eq!(
            Navidrome::item_url(&link),
            "https://music.example.com/app/#/album/3Kp%20a/show"
        );
    }
}
