//! stash-box GraphQL documents and response shapes, and their mapping to
//! the core catalog types. Only identifying and display fields are asked
//! for (spec 0007 "stashdb": id, title, date, duration, studio,
//! performers, images; no tags or fingerprints), plus a scene's `details`
//! (its description) in `findScene` only (spec 0008 Q14). Required
//! fields are not optional, so a changed shape fails as `Unknown`
//! (lesson 10). Unknown fields are ignored.
//!
//! Field names follow `graphql/schema` of stash-box (`types/scene.graphql`,
//! `performer.graphql`, `studio.graphql`, `user.graphql`, `version.graphql`,
//! `image.graphql`), read 2026-11-04.

use chrono::NaiveDate;
use serde::Deserialize;
use starr_core::catalog::{CatalogEntry, CatalogRef, MediaKind, NamedRef, SceneFacts};
use uuid::Uuid;

/// The image fields Starr reads (the URL is never used: images are fetched
/// by id from the instance's own host, see `crate::image`).
const IMAGE: &str = "fragment ImageParts on Image { id width height }";

const SCENE: &str = "fragment SceneParts on Scene { id title release_date duration \
    studio { id name } performers { performer { id name } } images { ...ImageParts } }";

/// Proves key and version and reads the account's roles (spec 0007 Q14).
pub const PROBE: &str = "query Probe { version { version } me { roles } }";

pub fn search_scenes() -> String {
    format!(
        "query SearchScenes($term: String!, $page: Int!, $perPage: Int!) {{ \
         searchScenes(term: $term, page: $page, per_page: $perPage) {{ count scenes {{ ...SceneParts }} }} }} \
         {SCENE} {IMAGE}"
    )
}

pub fn search_performers() -> String {
    format!(
        "query SearchPerformers($term: String!, $page: Int!, $perPage: Int!) {{ \
         searchPerformers(term: $term, page: $page, per_page: $perPage) {{ count performers {{ id name images {{ ...ImageParts }} }} }} }} \
         {IMAGE}"
    )
}

pub fn query_studios() -> String {
    format!(
        "query QueryStudios($input: StudioQueryInput!) {{ \
         queryStudios(input: $input) {{ count studios {{ id name images {{ ...ImageParts }} }} }} }} \
         {IMAGE}"
    )
}

pub fn query_scenes() -> String {
    format!(
        "query QueryScenes($input: SceneQueryInput!) {{ \
         queryScenes(input: $input) {{ count scenes {{ ...SceneParts }} }} }} \
         {SCENE} {IMAGE}"
    )
}

/// The scene page: the list fields plus `details`, which lists and search
/// never ask for (spec 0008 Q14 A).
pub fn find_scene() -> String {
    format!(
        "query FindScene($id: ID!) {{ findScene(id: $id) {{ ...SceneParts details }} }} {SCENE} {IMAGE}"
    )
}

/// Longest description handed out, in characters (spec 0008: the scene
/// page shows it capped, with "Show more").
pub const DETAILS_MAX_CHARS: usize = 2000;

/// A description as plain text: trimmed, control characters other than
/// line breaks and tabs dropped, at most [`DETAILS_MAX_CHARS`]; `None`
/// when nothing is left.
pub fn plain_details(raw: Option<String>) -> Option<String> {
    let text: String = raw?
        .trim()
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(DETAILS_MAX_CHARS)
        .collect();
    let text = text.trim_end().to_owned();
    (!text.is_empty()).then_some(text)
}

pub fn find_performer() -> String {
    format!(
        "query FindPerformer($id: ID!) {{ findPerformer(id: $id) {{ id name images {{ ...ImageParts }} }} }} {IMAGE}"
    )
}

pub fn find_studio() -> String {
    format!(
        "query FindStudio($id: ID!) {{ findStudio(id: $id) {{ id name images {{ ...ImageParts }} }} }} {IMAGE}"
    )
}

/// The GraphQL envelope.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlError {
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct ProbeData {
    pub version: VersionWire,
    pub me: Option<MeWire>,
}

#[derive(Debug, Deserialize)]
pub struct VersionWire {
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct MeWire {
    /// `null` when the server does not show them to this account.
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ImageWire {
    pub id: String,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Deserialize)]
pub struct NamedWire {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct AppearanceWire {
    pub performer: NamedWire,
}

#[derive(Debug, Deserialize)]
pub struct SceneWire {
    pub id: String,
    pub title: Option<String>,
    pub release_date: Option<String>,
    pub duration: Option<i64>,
    pub studio: Option<NamedWire>,
    #[serde(default)]
    pub performers: Vec<AppearanceWire>,
    #[serde(default)]
    pub images: Vec<ImageWire>,
    /// `findScene` only.
    #[serde(default)]
    pub details: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EntityWire {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub images: Vec<ImageWire>,
}

#[derive(Debug, Deserialize)]
pub struct ScenesPage {
    pub count: u64,
    pub scenes: Vec<SceneWire>,
}

#[derive(Debug, Deserialize)]
pub struct PerformersPage {
    pub count: u64,
    pub performers: Vec<EntityWire>,
}

#[derive(Debug, Deserialize)]
pub struct StudiosPage {
    pub count: u64,
    pub studios: Vec<EntityWire>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchScenesData {
    pub search_scenes: ScenesPage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPerformersData {
    pub search_performers: PerformersPage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryStudiosData {
    pub query_studios: StudiosPage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryScenesData {
    pub query_scenes: ScenesPage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindSceneData {
    pub find_scene: Option<SceneWire>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindPerformerData {
    pub find_performer: Option<EntityWire>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindStudioData {
    pub find_studio: Option<EntityWire>,
}

/// A stash-box id in canonical spelling (lower-case, hyphenated), or
/// `None` for anything else.
pub fn uuid(raw: &str) -> Option<Uuid> {
    Uuid::parse_str(raw)
        .ok()
        .filter(|id| id.hyphenated().to_string() == raw)
}

/// The canonical text of an id, as Starr's API and image proxy use it.
pub fn uuid_text(id: Uuid) -> String {
    id.hyphenated().to_string()
}

/// stash-box dates are partial ISO 8601 (`2019`, `2019-06`, `2019-06-15`):
/// a partial date counts as its first day (spec 0007 Q4); anything else is
/// `None`.
pub fn partial_date(raw: Option<&str>) -> Option<NaiveDate> {
    let raw = raw?.trim();
    let mut parts = raw.split('-');
    let year = parts.next()?;
    if year.len() != 4 {
        return None;
    }
    let year: i32 = year.parse().ok()?;
    let month: u32 = parts.next().map_or(Some(1), |m| m.parse().ok())?;
    let day: u32 = parts.next().map_or(Some(1), |d| d.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}

/// The image reference of an image id (ADR 0013 convention, ADR 0014:
/// `stashdb:<image uuid>`).
#[must_use]
pub fn image_ref(id: Uuid) -> String {
    format!("stashdb:{}", uuid_text(id))
}

/// The cover of a scene: the first landscape image (16:9 cards, design),
/// else the first; images with an invalid id are skipped.
fn landscape(images: &[ImageWire]) -> Option<String> {
    let valid = || images.iter().filter_map(|i| uuid(&i.id).map(|id| (id, i)));
    valid()
        .find(|(_, i)| i.width >= i.height)
        .or_else(|| valid().next())
        .map(|(id, _)| image_ref(id))
}

/// The first image with a valid id.
fn first(images: &[ImageWire]) -> Option<String> {
    images.iter().find_map(|i| uuid(&i.id)).map(image_ref)
}

fn named(kind: MediaKind, wire: NamedWire) -> Option<NamedRef> {
    let id = uuid(&wire.id)?;
    Some(NamedRef {
        item: CatalogRef {
            kind,
            id: uuid_text(id),
        },
        name: wire.name,
    })
}

fn entry(item: CatalogRef, title: String) -> CatalogEntry {
    CatalogEntry {
        item,
        original_title: title.clone(),
        title,
        original_language: String::new(),
        overview: String::new(),
        release_date: None,
        poster: None,
        backdrop: None,
        genre_ids: Vec::new(),
        rating: None,
        vote_count: 0,
        album: None,
        scene: None,
    }
}

impl SceneWire {
    /// The scene as a catalog entry; `None` without a valid id.
    pub fn into_entry(self) -> Option<CatalogEntry> {
        let id = uuid(&self.id)?;
        let item = CatalogRef {
            kind: MediaKind::Scene,
            id: uuid_text(id),
        };
        let mut e = entry(item, self.title.unwrap_or_default());
        e.release_date = partial_date(self.release_date.as_deref());
        e.poster = landscape(&self.images);
        e.scene = Some(SceneFacts {
            duration_seconds: self
                .duration
                .and_then(|d| u32::try_from(d).ok())
                .filter(|d| *d > 0),
            studio: self.studio.and_then(|s| named(MediaKind::Studio, s)),
            performers: self
                .performers
                .into_iter()
                .filter_map(|a| named(MediaKind::Performer, a.performer))
                .collect(),
            details: plain_details(self.details),
        });
        Some(e)
    }
}

impl EntityWire {
    /// A performer or studio as a catalog entry; `None` without a valid id.
    pub fn into_entry(self, kind: MediaKind) -> Option<CatalogEntry> {
        let id = uuid(&self.id)?;
        let mut e = entry(
            CatalogRef {
                kind,
                id: uuid_text(id),
            },
            self.name,
        );
        e.poster = first(&self.images);
        Some(e)
    }
}

/// Whether a page of `count` results starting at `offset` holding `len`
/// items is followed by more.
pub fn has_more(offset: u64, len: usize, count: u64) -> bool {
    len > 0 && offset + (len as u64) < count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_dates_count_as_their_first_day() {
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(partial_date(Some("2019")), d(2019, 1, 1));
        assert_eq!(partial_date(Some("2019-06")), d(2019, 6, 1));
        assert_eq!(partial_date(Some("2019-06-15")), d(2019, 6, 15));
        for bad in ["", "19", "2019-13", "2019-06-15-1", "x", "2019-06-31"] {
            assert_eq!(partial_date(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(partial_date(None), None);
    }

    #[test]
    fn ids_must_be_canonical() {
        assert!(uuid("7f3c1a52-0d4e-4b8a-9c61-2a5e8f9b0c11").is_some());
        for bad in [
            "7F3C1A52-0D4E-4B8A-9C61-2A5E8F9B0C11",
            "7f3c1a520d4e4b8a9c612a5e8f9b0c11",
            "../7f3c1a52-0d4e-4b8a-9c61-2a5e8f9b0c11",
            "",
        ] {
            assert!(uuid(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_scene_prefers_a_landscape_cover() {
        let image = |id: &str, width, height| ImageWire {
            id: id.into(),
            width,
            height,
        };
        let portrait = "11111111-1111-4111-8111-111111111111";
        let wide = "22222222-2222-4222-8222-222222222222";
        assert_eq!(
            landscape(&[image(portrait, 600, 900), image(wide, 1280, 720)]),
            Some(format!("stashdb:{wide}"))
        );
        assert_eq!(
            landscape(&[image("bad", 1, 1), image(portrait, 600, 900)]),
            Some(format!("stashdb:{portrait}"))
        );
        assert_eq!(landscape(&[]), None);
    }

    #[test]
    fn details_are_plain_capped_text() {
        assert_eq!(plain_details(None), None);
        assert_eq!(plain_details(Some("  \n ".into())), None);
        assert_eq!(
            plain_details(Some(" Details D.\n\nMore\u{0}\u{7}. ".into())).as_deref(),
            Some("Details D.\n\nMore.")
        );
        let long = "é".repeat(DETAILS_MAX_CHARS + 10);
        let capped = plain_details(Some(long)).unwrap();
        assert_eq!(capped.chars().count(), DETAILS_MAX_CHARS);
        // Markup stays text: the client renders it as text, never HTML.
        assert_eq!(
            plain_details(Some("<b>x</b>".into())).as_deref(),
            Some("<b>x</b>")
        );
    }

    #[test]
    fn only_the_scene_page_asks_for_details() {
        assert!(find_scene().contains("details"));
        for document in [search_scenes(), query_scenes()] {
            assert!(!document.contains("details"), "{document}");
        }
    }

    #[test]
    fn pages_continue_until_the_count() {
        assert!(has_more(0, 25, 26));
        assert!(!has_more(0, 25, 25));
        assert!(!has_more(25, 0, 100));
    }
}
