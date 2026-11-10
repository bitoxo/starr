# Lidarr 3.1.6.5079 fixtures

`recorded/` holds real responses (see its README). The files here are
**derived, not recorded**: they cover what a read-only recording cannot, and
are built from Lidarr's source (`develop`, read 2026-11-03) and the recorded
shapes.

| File | Derived from |
| --- | --- |
| `album_add_duplicate_400.json` | `AlbumExistsValidator` on `POST /album`; status and body unverified; `attemptedValue` is illustrative (the fake answers it for any duplicate) |
| `album_lookup_ok_computer.json`, `album_lookup_kid_a.json`, `album_lookup_in_rainbows.json` | `GET /album/lookup?term=lidarr:<rg>` for an artist Lidarr does not have (`AlbumLookupController`, `AlbumResource`); the embedded artist mirrors the recorded artist fields (no `mbId`, `nextAlbum`/`lastAlbum`) |
| `album_lookup_parachutes.json` | the same lookup for an album of an artist Lidarr has but an album it does not list: the recorded Parachutes entry of `GET /album?artistId=1` reshaped as a lookup result (no `id`, `artistId`/`profileId` 0, no `statistics`, `remoteCover` = its recorded cover `remoteUrl`, other fields as recorded), with the artist embedded as in `recorded/album_lookup_existing.json` |
| `queue_details.json` | `GET /queue/details?artistId=1` with entries (`QueueResource`); the recorded queue was empty |

`../2.14.5.4836/system_status.json` (the unsupported version for the probe
contract) is hand-written as well: the recorded status with another version.
