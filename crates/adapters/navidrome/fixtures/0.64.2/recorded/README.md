# Recorded Navidrome 0.64.2 responses

Read-only requests against a live Navidrome 0.64.2 (one library,
a few thousand albums, a handful of users), recorded 2026-11-06: `GET`s and
one admin login (`POST /auth/login`, which writes nothing). No password,
token, host name or other credential is in these files.

**Sanitised.** User ids, user names and display names, album, artist and
genre ids, album and artist names and image hashes were replaced by
placeholders (`recUser…`, `admin1`, `member1`…`member4`, `recAlbum…`,
"Album 1", "Artist 1", …); the login's `token`, `subsonicSalt` and
`subsonicToken` were redacted when recording. Album sizes, durations,
ReplayGain values and image dimensions are made up. Keys, types, nesting,
nulls, other numbers, dates, genres, the library's default name and path and the server
version are as recorded. `responses.json` holds each answer's status, the
headers that matter (`Content-Type`, `X-Total-Count`,
`X-Ratelimit-Limit`; `X-ND-Authorization` shown as `<renewed token>`) and
whether the answer renewed the token (`renewsTokenHeader`).

| File | Request |
| --- | --- |
| `rest_ping_no_creds.json` | `GET /rest/ping?v=1.16.1&c=starr&f=json` without credentials: HTTP 200, Subsonic error 10, `type: "navidrome"`, `serverVersion: "0.64.2 (…)"` |
| `auth_login.json` | `POST /auth/login` with the admin login (`X-Ratelimit-Limit: 5`) |
| `user_all.json` | `GET /api/user` (`X-Total-Count: 5`; every user carries `libraries`) |
| `user_one_member.json` | `GET /api/user/<id>` of the first non-admin |
| `user_one_member_library.json` | `GET /api/user/<id>/library` of the same user |
| `library_all.json` | `GET /api/library` (`X-Total-Count: 1`) |
| `album_page_lib1.json` | `GET /api/album?_start=0&_end=5&_sort=id&_order=ASC&library_id=1` (`X-Total-Count: 5517`) |
| `album_page_nofilter.json` | the same without `library_id` (identical: one library) |
| `album_page_lib999.json` | the same with `library_id=999` → `[]`, `X-Total-Count: 0` |

**No MusicBrainz IDs.** None of the albums has an `mbzAlbumId`,
`mbzReleaseGroupId` or `mbzAlbumArtistId` key (Navidrome omits empty ones)
and the Subsonic `musicBrainzId` is empty on all of them: the library's
files carry no MusicBrainz tags. The adapter reads the absent keys as "no
MBID" (`tests/recorded.rs`); the consequence is an accepted limitation
(spec 0006 Q7).

## Not recordable read-only

Albums with MBID tags, a second library, albums whose files are all
missing, and every account write are in `../derived/`, hand-written from
Navidrome's source.
