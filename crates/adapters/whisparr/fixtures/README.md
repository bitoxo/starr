# Whisparr v3 fixtures

**Recorded** (`3.6.5.1886/recorded/`, README there): read-only `GET`s
against a live Whisparr 3.6.5.1886 on 2026-11-06, sanitised. The
contract tests read status, profiles, root folders, the library entries,
lookups of a scene in the library and of an unknown one, and the (empty)
queues from there.

**Derived, not recorded:** the files below are hand-written from Whisparr's
source (tags `v3.1.0.2088` and `v3.1.0.2116` of branch `eros`, read
2026-11-04; identical in both tags except for `alternateTitles` in
`MovieResource`) for what a read-only recording cannot show. Titles, names
and IDs are neutral placeholders ("Scene B", "Studio S", made-up UUIDs);
nothing describes real content.

| File | Derived from |
| --- | --- |
| `2.2.0.231/system_status.json` | `SystemController.GetStatus`, `SystemResource.cs` with a v2 version (`v2-develop`, tag `v2.2.0-release.231`): outside the tested range |
| `3.1.0.2116/lookup_scene_b.json` | `GET /api/v3/lookup/scene?term=stash:<uuid>` for a scene Whisparr does not have (`SearchController.SearchScene` → `SkyHookProxy.SearchForNewScene`): a list of `SearchResource {id, foreignId, movie}` without library fields |
| `3.1.0.2116/movie_add_duplicate_400.json` | `MovieExistsValidator` on `foreignId` ("This item has already been added") → `ValidationException` → 400 with the failures (`WhisparrErrorPipeline`) |
| `3.1.0.2116/movie_add_excluded_400.json` | `AddMovieService.SetPropertiesAndValidate`: an excluded studio without an exclusion tag configured throws a `ValidationException` naming the studio |
| `3.1.0.2116/queue_details.json` | `GET /api/v3/queue/details?movieId=` with entries (`QueueDetailsController.GetQueue`, `QueueResource`; same as Radarr); every recorded queue was empty |
| `3.1.0.2116/system_status.json`, `qualityprofile.json`, `rootfolder.json` | `SystemController.GetStatus`/`SystemResource.cs` (`appName` is `BuildInfo.AppName`), `QualityProfileResource`, `RootFolderResource` (same as Radarr; with an inaccessible folder). The adapter's contract tests read the recorded ones; the server and worker tests (`crates/starr-server/tests`, `crates/starr-worker`) still seed their Whisparr fakes from these |

The derived library entry and unknown lookup of `3.1.0.2116/` were removed
once the recordings replaced them.
