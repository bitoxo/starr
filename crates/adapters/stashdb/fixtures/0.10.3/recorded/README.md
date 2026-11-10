# Recorded StashDB (stash-box 0.10.3) responses

Read-only GraphQL queries against `stashdb.org` with a fresh
READ-only account, recorded 2026-11-06, at about one
request per second. The queries are exactly the adapter's documents
(`src/wire.rs`). No API key or other credential is in these files.

**Sanitised.** StashDB is an adult catalog, so every scene title, performer
and studio name and every UUID (scenes, performers, studios, images) was
replaced by a neutral placeholder before commit: "Scene D", "Performer Q",
"Studio S", …; made-up UUIDs (`5ce0…` scenes, `9e7f…` performers, `57d1…`
studios, `1a6e…` images) consistent across these files and the Whisparr
recordings (`crates/adapters/whisparr/fixtures/3.6.5.1886/recorded/`):
"Scene D" and "Scene E" are scenes the recorded Whisparr has. Values that
could single out a real entry are **made up** too: release dates (a date
after the recording day stays in the future, as in the `DATE` lane) and
durations (the same in the Whisparr recordings), image widths and heights
(1920×1080 or 600×900 by orientation; the recorded `-1` stays), and the
`count` of a performer's, studio's or search term's scenes. Whole-catalog
counts (`TRENDING`, `DATE`, `CREATED_AT`), roles, the version and the
error texts are as recorded. Lists are **cut to three entries**.
`responses.json` holds each answer's status and the headers that matter
(`Content-Type`, `Cache-Control`, and `Content-Length` for the empty 401).

| File | Query |
| --- | --- |
| `probe.json` | `query Probe { version { version } me { roles } }` → `v0.10.3`, roles `["READ"]` |
| `probe_without_key.json` | the probe without `ApiKey`: HTTP 200, a GraphQL error `"not authorized"` per field, `data: null` |
| `probe_bad_key` (only in `responses.json`) | the probe with a wrong key: **HTTP 401, empty body** |
| `find_scene_d.json`, `find_scene_e.json` | `findScene(id)` of the two scenes |
| `find_scene_unknown.json` | `findScene(id)` of a random UUID → `{findScene: null}` |
| `query_scenes_trending.json` | `queryScenes(sort: TRENDING, direction: DESC, per_page: 25)` (count 188,875) |
| `query_scenes_date.json` | the same with `sort: DATE` (count 1,121,679; starts with scenes dated after the recording day) |
| `query_scenes_created_at.json` | the same with `sort: CREATED_AT` |
| `find_performer.json` | `findPerformer(id)` of Scene D's first performer ("Performer Q") |
| `query_scenes_performer_date.json` | `queryScenes(performers: {value: [Performer Q], modifier: INCLUDES}, sort: DATE, per_page: 100)` (count made up: 250; recorded in the thousands) |
| `find_studio.json` | `findStudio(id)` of Scene D's studio ("Studio S"; its logo reports width and height `-1`) |
| `query_scenes_studio_date.json` | the same by `studios` (count made up: 300; recorded in the hundreds) |
| `search_scenes.json` | `searchScenes(term, page: 1, per_page: 10)`, term = a word of the studio's name |
| `search_performers.json` | `searchPerformers(term, page: 1, per_page: 10)`, term = a word of the performer's name |
| `query_studios.json` | `queryStudios(input: {name: term, page: 1, per_page: 10})` (one studio without images) |
| `image_without_key_{300,600}` (only in `responses.json`) | `GET /images/<Scene D's image>?size=300|600` without the key: **200 `image/jpeg` directly, no redirect**, `Cache-Control: max-age=604800000`; the body was not kept |

## Not recorded

A `VOTE` or `EDIT` role, partial or missing release dates, portrait covers,
invalid ids, other GraphQL errors and a CDN redirect are in `../../` and
derived from the stash-box schema (each says so in `extensions.fixture`).

A scene's `details` (spec 0008 Q14) is not recorded either:
`../../find_scene.json` carries a placeholder description ("Details D: …",
with surrounding whitespace and a control character), derived from the
schema.

Live check (read-only, 2026-11-07): `queryScenes` combines `text`
with a studio filter as AND (no recording kept; the fake answers the
filtered query with its usual scene page, whose scenes name that studio).
