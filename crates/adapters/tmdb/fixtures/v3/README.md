# TMDB v3 fixtures

**Recorded** (`recorded/`): bodies from the live API on 2026-11-01
(`../../record-fixtures.sh`), exercised by `tests/recorded.rs`. The details
there were recorded before spec 0008 appended `credits`/`aggregate_credits`,
so they carry no cast or crew.

**Derived, not recorded** (the files directly in this directory): synthetic
scenarios built from the recorded shapes and TMDB's documented responses
for `tests/contract.rs` and the server tests (duplicates within a page,
empty overviews, odd values, errors). Additions of spec 0008 slice 2, from
TMDB's API reference (read 2026-11-06), with public catalog data or made-up
values:

| File | Added |
| --- | --- |
| `movie_27205.json` | `credits` (14 cast in reverse billing order, a director who is also the writer, crew of other departments), `production_companies`, a digital release (type 4) in CH. Profile paths are made up. |
| `tv_1399.json` | `aggregate_credits` (cast with `roles`, writers and a producer credit of the same person), `networks`, `production_companies`, `next_episode_to_air: null` |
| `tv_1399_season_1.json` | `GET /3/tv/1399/season/1`: episodes out of order, one without air date and runtime |

The server tests also serve `discover_tv.json` for `tv/on_the_air` and
`recorded/trending_tv_week_page1.json` for the series Trending lane.
`record-fixtures.sh` asks for the new appends, the season and the series
lists, so the next recording replaces these derivations.

Live check (read-only, 2026-11-07): `discover/tv` with the
"Returning soon" window returns mostly running weekly shows (19 of the top
20); the lane was dropped (`docs/concept/capabilities.md`), and its call is
no longer in `record-fixtures.sh`.
