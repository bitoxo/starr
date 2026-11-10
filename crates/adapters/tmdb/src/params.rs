//! Pure request building and result shaping for the TMDB catalog: which
//! endpoint and parameters serve a list (spec 0003 D4), the page cap,
//! relevance filtering and the English fallback (D5).

use std::collections::HashMap;

use chrono::{Days, NaiveDate};
use starr_core::catalog::{
    CatalogEntry, CatalogRef, GenreRef, ListFilter, ListKind, MediaKind, Relevance,
};

/// TMDB serves at most this page (10,000 results); page 501 is a 400
/// `status_code 22` although `total_pages` may claim more.
pub const PAGE_CAP: u32 = 500;

/// D4: "Top rated" needs this many votes.
pub const TOP_RATED_MIN_VOTES_MOVIE: u32 = 5000;
pub const TOP_RATED_MIN_VOTES_SERIES: u32 = 1000;

/// D4: series hide Talk, News, Soap and Reality by default (TV genre ids,
/// capabilities.md). Slice 2 uses this as the category default.
pub const DEFAULT_SERIES_EXCLUDED_GENRES: [u64; 4] = [10767, 10763, 10766, 10764];

/// "Upcoming" movies count as first releases if their primary (worldwide)
/// release is at most this old; older titles are re-releases.
const UPCOMING_FIRST_RELEASE_DAYS: u64 = 365;
/// "On your services": movies released within this window …
const ON_SERVICES_MOVIE_DAYS: u64 = 365;
/// … and series with an episode aired within this window.
const ON_SERVICES_SERIES_DAYS: u64 = 90;

/// Lists TMDB serves beyond the Discover lanes of its descriptor: the
/// series lists of a category page and the "release date" sort of its
/// grid (spec 0008 slice 2).
pub const PAGE_LISTS: [ListKind; 2] = [ListKind::New, ListKind::OnTheAir];

/// The language sent to TMDB. `de-CH` is listed by TMDB but answers with
/// English titles and empty overviews, so it is sent as `de-DE`
/// (capabilities.md, TMDB "Language").
pub fn tmdb_language(language: &str) -> String {
    match language {
        "de-CH" => "de-DE".to_owned(),
        other => other.to_owned(),
    }
}

/// English needs no fallback.
pub fn is_english(language: &str) -> bool {
    language == "en" || language.starts_with("en-")
}

pub const FALLBACK_LANGUAGE: &str = "en-US";

/// The kinds TMDB serves.
pub const KINDS: [MediaKind; 2] = [MediaKind::Movie, MediaKind::Series];

/// TMDB's path segment for `kind`; `None` for kinds TMDB does not serve
/// (music).
pub const fn tmdb_kind(kind: MediaKind) -> Option<&'static str> {
    match kind {
        MediaKind::Movie => Some("movie"),
        MediaKind::Series => Some("tv"),
        MediaKind::Artist
        | MediaKind::Album
        | MediaKind::Scene
        | MediaKind::Performer
        | MediaKind::Studio => None,
    }
}

/// Next page cursor: `None` on the last page, which includes the cap.
pub fn next_page(page: u32, total_pages: u32) -> Option<u32> {
    (page < total_pages.min(PAGE_CAP)).then_some(page + 1)
}

/// Whether `page` can be requested at all.
pub fn page_in_range(page: u32) -> bool {
    (1..=PAGE_CAP).contains(&page)
}

fn joined<T: ToString>(values: &[T], separator: &str) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

fn days_before(today: NaiveDate, days: u64) -> String {
    today
        .checked_sub_days(Days::new(days))
        .unwrap_or(today)
        .to_string()
}

/// Whether TMDB serves `list` at all (lanes and page lists).
fn served(list: ListKind) -> bool {
    crate::DESCRIPTOR.lists.contains(&list) || PAGE_LISTS.contains(&list)
}

/// Whether `entry` passes the genre of `filter`: the filter of lists TMDB
/// cannot filter by genre, and a safety net for Discover.
pub fn passes_filter(entry: &CatalogEntry, filter: Option<&ListFilter>) -> bool {
    match filter {
        Some(ListFilter::Genre(GenreRef::Id(genre))) => entry.genre_ids.contains(genre),
        _ => true,
    }
}

/// One list page to ask TMDB for.
#[derive(Debug, Clone, Copy)]
pub struct ListRequest<'a> {
    pub kind: MediaKind,
    pub list: ListKind,
    pub page: u32,
    pub language: &'a str,
    pub relevance: &'a Relevance,
    pub filter: Option<&'a ListFilter>,
}

/// Path and query for one list page, or `None` if the relevance cannot
/// serve the list ("On your services" without region or providers, a
/// provider filter without a provider region), for kinds and lists TMDB
/// does not serve (music, "Fresh releases", series lists of movies), and
/// for filters TMDB cannot apply to the list (a provider on Trending or
/// "On the air", a genre by name). A genre filter on Trending and "On the
/// air" is applied to the results instead ([`passes_filter`]).
pub fn list_request(
    request: &ListRequest<'_>,
    today: NaiveDate,
) -> Option<(String, Vec<(&'static str, String)>)> {
    let ListRequest {
        kind,
        list,
        page,
        language,
        relevance,
        filter,
    } = *request;
    let tmdb = tmdb_kind(kind)?;
    if !served(list) {
        return None;
    }
    let series_only = list == ListKind::OnTheAir;
    if series_only && kind != MediaKind::Series {
        return None;
    }
    let provider = match filter {
        Some(ListFilter::Provider(id)) => Some(*id),
        Some(ListFilter::Genre(GenreRef::Name(_))) => return None,
        Some(ListFilter::Genre(GenreRef::Id(_))) | None => None,
    };
    let mut query: Vec<(&'static str, String)> = vec![
        ("language", tmdb_language(language)),
        ("page", page.to_string()),
    ];
    // Trending and "On the air" take no filters at all; relevance (and a
    // genre filter) is applied to the results (capabilities.md, TMDB
    // "Lists").
    match list {
        ListKind::Trending if provider.is_none() => {
            return Some((format!("3/trending/{tmdb}/week"), query));
        }
        ListKind::OnTheAir if provider.is_none() => {
            return Some(("3/tv/on_the_air".to_owned(), query));
        }
        ListKind::Trending | ListKind::OnTheAir => return None,
        _ => {}
    }

    query.push(("include_adult", "false".to_owned()));
    if !relevance.original_languages.is_empty() {
        // `|` is OR.
        query.push((
            "with_original_language",
            joined(&relevance.original_languages, "|"),
        ));
    }
    if !relevance.excluded_genres.is_empty() {
        // Semantics of `,` vs `|` for exclusion are unverified; results are
        // filtered again in Starr either way.
        query.push(("without_genres", joined(&relevance.excluded_genres, ",")));
    }
    if let Some(ListFilter::Genre(GenreRef::Id(genre))) = filter {
        query.push(("with_genres", genre.to_string()));
    }
    let sort = match (list, kind) {
        (ListKind::TopRated, _) => "vote_average.desc",
        (ListKind::New, MediaKind::Series) => "first_air_date.desc",
        (ListKind::New, _) => "primary_release_date.desc",
        _ => "popularity.desc",
    };
    query.push(("sort_by", sort.to_owned()));
    // "On <provider>": the provider's subscription in the provider region
    // (spec 0008 Q13 B); without a region there is nothing to ask for.
    if let Some(provider) = provider
        && list != ListKind::OnServices
    {
        query.push(("watch_region", relevance.provider_region.clone()?));
        query.push(("with_watch_providers", provider.to_string()));
        query.push(("with_watch_monetization_types", "flatrate".to_owned()));
    }

    list_window(list, kind, relevance, provider, today, &mut query)?;
    Some((format!("3/discover/{tmdb}"), query))
}

/// The date window and provider parameters of a Discover list; `None`
/// when the relevance cannot serve it.
fn list_window(
    list: ListKind,
    kind: MediaKind,
    relevance: &Relevance,
    provider: Option<u64>,
    today: NaiveDate,
    query: &mut Vec<(&'static str, String)>,
) -> Option<()> {
    let today_s = today.to_string();
    match (list, kind) {
        (ListKind::Trending | ListKind::Popular, _) => {}
        (ListKind::Upcoming, MediaKind::Movie) => {
            if let Some(region) = &relevance.release_region {
                // Theatrical (limited or wide) release in the region from
                // today on, and no re-releases of old titles.
                query.push(("region", region.clone()));
                query.push(("with_release_type", "2|3".to_owned()));
                query.push(("release_date.gte", today_s));
                query.push((
                    "primary_release_date.gte",
                    days_before(today, UPCOMING_FIRST_RELEASE_DAYS),
                ));
            } else {
                query.push(("primary_release_date.gte", today_s));
            }
        }
        (ListKind::Upcoming, MediaKind::Series) => {
            // New series only; TMDB has no regional premiere dates for TV.
            query.push(("first_air_date.gte", today_s));
        }
        // Excluded above.
        (
            ListKind::Upcoming,
            MediaKind::Artist
            | MediaKind::Album
            | MediaKind::Scene
            | MediaKind::Performer
            | MediaKind::Studio,
        )
        | (ListKind::FreshReleases, _) => {
            return None;
        }
        (ListKind::New, MediaKind::Series) => {
            query.push(("first_air_date.lte", today_s));
        }
        (ListKind::New, _) => {
            query.push(("primary_release_date.lte", today_s));
        }
        // Returned above.
        (ListKind::OnTheAir, _) => return None,
        (ListKind::OnServices, _) => {
            let region = relevance.provider_region.as_ref()?;
            // A provider filter narrows "On your services" to that one.
            let providers = match provider {
                Some(id) => id.to_string(),
                None if relevance.providers.is_empty() => return None,
                None => joined(&relevance.providers, "|"),
            };
            query.push(("watch_region", region.clone()));
            query.push(("with_watch_providers", providers));
            query.push((
                "with_watch_monetization_types",
                "flatrate|free|ads".to_owned(),
            ));
            if kind == MediaKind::Series {
                query.push(("air_date.gte", days_before(today, ON_SERVICES_SERIES_DAYS)));
            } else {
                query.push((
                    "primary_release_date.gte",
                    days_before(today, ON_SERVICES_MOVIE_DAYS),
                ));
            }
        }
        (ListKind::TopRated, _) => {
            let min = if kind == MediaKind::Series {
                TOP_RATED_MIN_VOTES_SERIES
            } else {
                TOP_RATED_MIN_VOTES_MOVIE
            };
            query.push(("vote_count.gte", min.to_string()));
        }
    }
    Some(())
}

/// Relevance applied to results: original language and excluded genres.
/// The only filter for Trending; a safety net for Discover.
pub fn is_relevant(entry: &CatalogEntry, relevance: &Relevance) -> bool {
    let language_ok = relevance.original_languages.is_empty()
        || relevance
            .original_languages
            .iter()
            .any(|l| l == &entry.original_language);
    let genres_ok = !entry
        .genre_ids
        .iter()
        .any(|g| relevance.excluded_genres.contains(g));
    language_ok && genres_ok
}

/// TMDB returns an empty overview (and the original title) when no
/// translation exists, without an English fallback.
pub fn needs_fallback(entry: &CatalogEntry) -> bool {
    entry.title.trim().is_empty() || entry.overview.trim().is_empty()
}

/// Fill empty fields of `entry` from its English version. A title that is
/// just the original title in another language than the requested one is
/// replaced by the English title as well.
pub fn apply_fallback(entry: &mut CatalogEntry, english: &CatalogEntry, language: &str) {
    let requested = language.split('-').next().unwrap_or(language);
    let untranslated_title = entry.title.trim().is_empty()
        || (entry.title == entry.original_title && entry.original_language != requested);
    if untranslated_title && !english.title.trim().is_empty() {
        entry.title.clone_from(&english.title);
    }
    if entry.overview.trim().is_empty() {
        entry.overview.clone_from(&english.overview);
    }
}

/// [`apply_fallback`] for every entry with an English version in `english`.
/// Matched by kind and id: a movie and a series may share a TMDB id.
pub fn apply_fallbacks(entries: &mut [CatalogEntry], english: Vec<CatalogEntry>, language: &str) {
    let by_ref: HashMap<CatalogRef, CatalogEntry> =
        english.into_iter().map(|e| (e.item.clone(), e)).collect();
    for entry in entries.iter_mut().filter(|e| needs_fallback(e)) {
        if let Some(en) = by_ref.get(&entry.item) {
            apply_fallback(entry, en, language);
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn lreq(
        kind: MediaKind,
        list: ListKind,
        page: u32,
        language: &str,
        relevance: &Relevance,
        today: NaiveDate,
    ) -> Option<(String, Vec<(&'static str, String)>)> {
        filtered(kind, list, page, language, relevance, today, None)
    }

    fn filtered(
        kind: MediaKind,
        list: ListKind,
        page: u32,
        language: &str,
        relevance: &Relevance,
        today: NaiveDate,
        filter: Option<&ListFilter>,
    ) -> Option<(String, Vec<(&'static str, String)>)> {
        list_request(
            &ListRequest {
                kind,
                list,
                page,
                language,
                relevance,
                filter,
            },
            today,
        )
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
    }

    fn param<'a>(q: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
        q.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    fn relevance() -> Relevance {
        Relevance {
            original_languages: vec!["en".into(), "de".into()],
            provider_region: Some("CH".into()),
            providers: vec![8, 337],
            release_region: Some("CH".into()),
            excluded_genres: vec![10767, 10763],
            ..Relevance::default()
        }
    }

    fn entry(id: &str, lang: &str, genres: &[u64]) -> CatalogEntry {
        CatalogEntry {
            item: CatalogRef {
                kind: MediaKind::Movie,
                id: id.into(),
            },
            title: format!("t{id}"),
            original_title: format!("t{id}"),
            original_language: lang.into(),
            overview: "o".into(),
            release_date: None,
            poster: None,
            backdrop: None,
            genre_ids: genres.to_vec(),
            rating: None,
            vote_count: 0,
            album: None,
            scene: None,
        }
    }

    #[test]
    fn page_cap_ends_the_list() {
        assert_eq!(next_page(1, 3), Some(2));
        assert_eq!(next_page(3, 3), None);
        assert_eq!(next_page(499, 1001), Some(500));
        assert_eq!(next_page(500, 1001), None);
        assert!(page_in_range(500));
        assert!(!page_in_range(501));
        assert!(!page_in_range(0));
    }

    #[test]
    fn trending_has_no_filter_parameters() {
        let (path, q) = lreq(
            MediaKind::Series,
            ListKind::Trending,
            2,
            "en-US",
            &relevance(),
            today(),
        )
        .unwrap();
        assert_eq!(path, "3/trending/tv/week");
        assert_eq!(q.len(), 2, "{q:?}");
        assert_eq!(param(&q, "page"), Some("2"));
    }

    #[test]
    fn music_kinds_and_music_lists_are_not_served() {
        for kind in [MediaKind::Artist, MediaKind::Album] {
            for list in ListKind::ALL {
                assert_eq!(lreq(kind, list, 1, "en-US", &relevance(), today()), None);
            }
        }
        for kind in KINDS {
            assert_eq!(
                lreq(
                    kind,
                    ListKind::FreshReleases,
                    1,
                    "en-US",
                    &relevance(),
                    today()
                ),
                None
            );
        }
    }

    #[test]
    fn discover_lists_carry_languages_and_genre_exclusions() {
        for list in [
            ListKind::Popular,
            ListKind::Upcoming,
            ListKind::OnServices,
            ListKind::TopRated,
        ] {
            let (path, q) =
                lreq(MediaKind::Movie, list, 1, "en-US", &relevance(), today()).unwrap();
            assert_eq!(path, "3/discover/movie");
            assert_eq!(param(&q, "with_original_language"), Some("en|de"));
            assert_eq!(param(&q, "without_genres"), Some("10767,10763"));
            assert_eq!(param(&q, "include_adult"), Some("false"));
        }
    }

    #[test]
    fn upcoming_movies_are_first_releases_in_the_release_region() {
        let (_, q) = lreq(
            MediaKind::Movie,
            ListKind::Upcoming,
            1,
            "en-US",
            &relevance(),
            today(),
        )
        .unwrap();
        assert_eq!(param(&q, "region"), Some("CH"));
        assert_eq!(param(&q, "with_release_type"), Some("2|3"));
        assert_eq!(param(&q, "release_date.gte"), Some("2026-11-01"));
        assert_eq!(param(&q, "primary_release_date.gte"), Some("2025-11-01"));
        let no_region = Relevance {
            release_region: None,
            ..relevance()
        };
        let (_, q) = lreq(
            MediaKind::Movie,
            ListKind::Upcoming,
            1,
            "en-US",
            &no_region,
            today(),
        )
        .unwrap();
        assert_eq!(param(&q, "region"), None);
        assert_eq!(param(&q, "primary_release_date.gte"), Some("2026-11-01"));
    }

    #[test]
    fn on_services_needs_region_and_providers() {
        let (_, q) = lreq(
            MediaKind::Series,
            ListKind::OnServices,
            1,
            "en-US",
            &relevance(),
            today(),
        )
        .unwrap();
        assert_eq!(param(&q, "watch_region"), Some("CH"));
        assert_eq!(param(&q, "with_watch_providers"), Some("8|337"));
        assert_eq!(param(&q, "air_date.gte"), Some("2026-08-03"));
        for r in [
            Relevance {
                providers: vec![],
                ..relevance()
            },
            Relevance {
                provider_region: None,
                ..relevance()
            },
        ] {
            assert!(
                lreq(
                    MediaKind::Movie,
                    ListKind::OnServices,
                    1,
                    "en-US",
                    &r,
                    today()
                )
                .is_none()
            );
        }
    }

    #[test]
    fn top_rated_has_minimum_votes_per_kind() {
        let q = |kind| {
            lreq(
                kind,
                ListKind::TopRated,
                1,
                "en-US",
                &Relevance::default(),
                today(),
            )
            .unwrap()
            .1
        };
        let movie = q(MediaKind::Movie);
        assert_eq!(param(&movie, "vote_count.gte"), Some("5000"));
        assert_eq!(param(&movie, "sort_by"), Some("vote_average.desc"));
        assert_eq!(param(&movie, "with_original_language"), None);
        assert_eq!(param(&q(MediaKind::Series), "vote_count.gte"), Some("1000"));
    }

    #[test]
    fn de_ch_is_sent_as_de_de() {
        let (_, q) = lreq(
            MediaKind::Movie,
            ListKind::Popular,
            1,
            "de-CH",
            &Relevance::default(),
            today(),
        )
        .unwrap();
        assert_eq!(param(&q, "language"), Some("de-DE"));
        assert!(is_english("en-US"));
        assert!(is_english("en"));
        assert!(!is_english("de-DE"));
    }

    #[test]
    fn series_lists_of_a_category_page() {
        let (path, q) = lreq(
            MediaKind::Series,
            ListKind::OnTheAir,
            1,
            "en-US",
            &relevance(),
            today(),
        )
        .unwrap();
        assert_eq!(path, "3/tv/on_the_air");
        assert_eq!(q.len(), 2, "no filters: {q:?}");
        // Movies have no series lists.
        assert_eq!(
            lreq(
                MediaKind::Movie,
                ListKind::OnTheAir,
                1,
                "en-US",
                &relevance(),
                today()
            ),
            None
        );
    }

    #[test]
    fn release_date_sort_is_newest_first_up_to_today() {
        let (_, q) = lreq(
            MediaKind::Movie,
            ListKind::New,
            1,
            "en-US",
            &relevance(),
            today(),
        )
        .unwrap();
        assert_eq!(param(&q, "sort_by"), Some("primary_release_date.desc"));
        assert_eq!(param(&q, "primary_release_date.lte"), Some("2026-11-01"));
        let (_, q) = lreq(
            MediaKind::Series,
            ListKind::New,
            1,
            "en-US",
            &relevance(),
            today(),
        )
        .unwrap();
        assert_eq!(param(&q, "sort_by"), Some("first_air_date.desc"));
        assert_eq!(param(&q, "first_air_date.lte"), Some("2026-11-01"));
    }

    #[test]
    fn provider_filter_is_the_providers_subscription_in_the_region() {
        let on = ListFilter::Provider(337);
        let (path, q) = filtered(
            MediaKind::Movie,
            ListKind::Popular,
            1,
            "en-US",
            &relevance(),
            today(),
            Some(&on),
        )
        .unwrap();
        assert_eq!(path, "3/discover/movie");
        assert_eq!(param(&q, "watch_region"), Some("CH"));
        assert_eq!(param(&q, "with_watch_providers"), Some("337"));
        assert_eq!(param(&q, "with_watch_monetization_types"), Some("flatrate"));
        // "On your services" narrowed to one of them, parameters once.
        let (_, q) = filtered(
            MediaKind::Movie,
            ListKind::OnServices,
            1,
            "en-US",
            &relevance(),
            today(),
            Some(&on),
        )
        .unwrap();
        assert_eq!(param(&q, "with_watch_providers"), Some("337"));
        assert_eq!(
            q.iter()
                .filter(|(k, _)| *k == "with_watch_providers")
                .count(),
            1
        );
        // Without a provider region, and on lists TMDB cannot filter.
        let no_region = Relevance {
            provider_region: None,
            ..relevance()
        };
        for (l, r) in [
            (ListKind::Popular, &no_region),
            (ListKind::Trending, &relevance()),
        ] {
            assert_eq!(
                filtered(MediaKind::Movie, l, 1, "en-US", r, today(), Some(&on)),
                None,
                "{l:?}"
            );
        }
        assert_eq!(
            filtered(
                MediaKind::Series,
                ListKind::OnTheAir,
                1,
                "en-US",
                &relevance(),
                today(),
                Some(&on)
            ),
            None
        );
    }

    #[test]
    fn genre_filter_by_id_only() {
        let drama = ListFilter::Genre(GenreRef::Id(18));
        let (_, q) = filtered(
            MediaKind::Movie,
            ListKind::TopRated,
            1,
            "en-US",
            &relevance(),
            today(),
            Some(&drama),
        )
        .unwrap();
        assert_eq!(param(&q, "with_genres"), Some("18"));
        // Trending takes the genre on its results.
        let (path, q) = filtered(
            MediaKind::Movie,
            ListKind::Trending,
            1,
            "en-US",
            &relevance(),
            today(),
            Some(&drama),
        )
        .unwrap();
        assert_eq!(path, "3/trending/movie/week");
        assert_eq!(param(&q, "with_genres"), None);
        assert!(passes_filter(&entry("1", "en", &[18]), Some(&drama)));
        assert!(!passes_filter(&entry("2", "en", &[35]), Some(&drama)));
        assert!(passes_filter(&entry("2", "en", &[35]), None));
        // TMDB keys genres by id; a name is not one of its genres.
        let by_name = ListFilter::Genre(GenreRef::Name("drama".into()));
        assert_eq!(
            filtered(
                MediaKind::Movie,
                ListKind::Popular,
                1,
                "en-US",
                &relevance(),
                today(),
                Some(&by_name)
            ),
            None
        );
    }

    #[test]
    fn relevance_filters_language_and_genres() {
        let r = relevance();
        assert!(is_relevant(&entry("1", "en", &[18]), &r));
        assert!(!is_relevant(&entry("2", "ko", &[18]), &r));
        assert!(!is_relevant(&entry("3", "de", &[18, 10767]), &r));
        assert!(is_relevant(
            &entry("4", "ko", &[10767]),
            &Relevance::default()
        ));
    }

    #[test]
    fn fallback_fills_empty_overview_and_untranslated_title() {
        let mut e = entry("1", "ko", &[]);
        e.title = "기생충".into();
        e.original_title = "기생충".into();
        e.overview = String::new();
        let mut en = entry("1", "ko", &[]);
        en.title = "Parasite".into();
        en.overview = "A poor family…".into();
        assert!(needs_fallback(&e));
        apply_fallbacks(std::slice::from_mut(&mut e), vec![en], "de-DE");
        assert_eq!(e.title, "Parasite");
        assert_eq!(e.overview, "A poor family…");

        // A translated title stays, even if the overview came from English.
        let mut e = entry("2", "en", &[]);
        e.title = "Der Pate".into();
        e.original_title = "The Godfather".into();
        e.overview = String::new();
        let mut en = entry("2", "en", &[]);
        en.title = "The Godfather".into();
        en.overview = "Mafia".into();
        apply_fallback(&mut e, &en, "de-DE");
        assert_eq!(e.title, "Der Pate");
        assert_eq!(e.overview, "Mafia");
    }
}
