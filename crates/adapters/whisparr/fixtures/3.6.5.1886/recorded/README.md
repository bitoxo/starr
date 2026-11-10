# Recorded Whisparr 3.6.5.1886 responses

Read-only `GET`s against a live Whisparr 3.6.5.1886 (branch
`eros`, hotio image, SQLite), recorded 2026-11-06. No API
key, host name or other credential is in these files.

**Sanitised.** The library holds real adult content, so every title,
performer and studio name, StashDB UUID, ThePornDB id, overview, genre,
path, file name, website and image URL was replaced by a neutral
placeholder before commit: scenes "Scene A", "Scene D", "Scene E";
performers "Performer P", "Performer Q", …; studio "Studio S"; made-up UUIDs
(`5ce0…` scenes, `9e7f…` performers, `57d1…` studios, `1a6e…` images,
`7bdb…` ThePornDB) that stay consistent across these files and the StashDB
recordings (`crates/adapters/stashdb/fixtures/0.10.3/recorded/`), so
cross-references still match; paths under `/data/private`; URLs on
`example.invalid`. Values that could single out a real scene are made up as
well: release date, year and running time (consistent with the StashDB
recordings), sizes, file dates, search and add times, bitrates and frame
rate. Every other key, type, nesting, enum value (`itemType`, `status`,
`coverType`, quality names, …), `null` and id is as recorded. `genres` are
cut to two placeholder tags. The performer and studio samples keep **only
the monitoring fields** (`name`/`title`, `foreignId`, `monitored`,
`moviesMonitored`, `id`): their other fields (personal attributes, scene
counts, sizes, years) would point at the real entries. `responses.json` holds
each answer's status and the headers that matter (`Content-Type`,
`X-Application-Version`).

| File | Request (`/api/v3/...`) |
| --- | --- |
| `system_status.json` | `GET system/status` (`appName` "Whisparr", `version` 3.6.5.1886, `branch` "eros") |
| `qualityprofile.json` | `GET qualityprofile` (ids 4 "1080p", 5 "4K") |
| `rootfolder.json` | `GET rootfolder` (one folder, id 2) |
| `movie_by_stashid_unmonitored.json` | `GET movie?stashId=<uuid>` of an unmonitored scene without file ("Scene A", id 29; the contract fake's library) |
| `movie_by_stashid_with_file.json` | the same for a monitored scene with its file ("Scene D", id 1; with `movieFile`) |
| `movie_by_stashid_monitored_no_file.json` | the same for a monitored scene without file ("Scene E", id 3759) |
| `movie_by_stashid_unknown.json` | `GET movie?stashId=<random uuid>` → `[]` |
| `lookup_scene_known.json` | `GET lookup/scene?term=stash:<uuid of Scene D>`: `[{foreignId, id, isExisting: true, movie}]`, `movie` without `hasFile` and `rootFolderPath` |
| `lookup_scene_unknown.json` | `GET lookup/scene?term=stash:<random uuid>` → `[]` |
| `queue_details_{unmonitored,with_file,monitored_no_file}.json` | `GET queue/details?movieId=<id>` → `[]` |
| `queue_details_all.json` | `GET queue/details` → `[]` (the whole queue was empty) |
| `movie_all.json` | `GET movie` — **sample**: the three scenes above, identical to their `?stashId=` entries (the full answer had 4257 entries, every one `itemType: "scene"`) |
| `performer_all.json` | `GET performer` — **sample**: three performers of those scenes (of 3949, all `monitored` and `moviesMonitored` false) |
| `studio_all.json` | `GET studio` — **sample**: the studio of those scenes (of 34; 12 `monitored: true`, set by the operator; all `moviesMonitored` false) |

## Not recordable read-only

The answer for a scene Whisparr's metadata knows but the library lacks, add
answers (201, duplicate and exclusion 400s) and a queue with entries are in
`../../3.1.0.2116/` and derived from Whisparr's source (`../../README.md`).
