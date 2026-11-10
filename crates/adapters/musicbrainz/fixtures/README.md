# MusicBrainz and ListenBrainz fixtures

**Derived, not recorded**: every file is hand-written from the shapes in
the official API documentation (`MusicBrainz` `/ws/2` JSON, `ListenBrainz`
API docs); Starr's tests never call either service. See the header of
`tests/contract.rs` for what is inferred.

Additions of spec 0008 slice 2 (read in the API docs 2026-11-06):

| File | Added |
| --- | --- |
| `ws2/release_group_ok_computer.json` | `genres[] {id, name, count, disambiguation}` (`inc=genres`), with ties in count |
| `ws2/release_ok_computer_media.json` | `media[].tracks[]` (`inc=recordings+artist-credits`: `position`, `number`, `title`, `length` in ms, `artist-credit`, `recording`), a bonus disc listed out of order with one track credited to another artist and one without length; `label-info[]` (`inc=labels`). Track titles are public core data (CC0); ids are made up. |
| `ws2/artist_radiohead.json` | `genres[]` (`inc=genres`) |
| `ws2/search_release_group_genre_rock.json` | a genre lane's search answer (the release-group search shape); a third, made-up release group without listens (dropped by the ranking) |
| `listenbrainz/popularity_release_group_rock.json` | `POST /1/popularity/release-group` for that lane, in the shape seen live 2026-11-07: one object per MBID with `total_listen_count`/`total_user_count`, `null` for the unknown one; counts made up |

Genres are supplementary data under CC BY-NC-SA (spec 0008 Q17 B); the
fixtures hold only a few genre names.

Live check (read-only, 2026-11-07): a genre search returns only
releases carrying the exact tag, in search-score order (no popularity), and
MusicBrainz may answer 503 while a refresh runs; the fake for that case is
in `starr-worker` (`refresh_lists_stops_at_a_503_and_keeps_the_cached_lanes`).
