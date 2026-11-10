#!/usr/bin/env bash
# Records TMDB contract fixtures into fixtures/v3/recorded/ (optional; CI
# and everyone else run on the committed files).
#
#   TMDB_TOKEN_FILE=~/.config/tmdb-token crates/adapters/tmdb/record-fixtures.sh [headers-dir]
#
# The file holds a TMDB API Read Access Token. It is read at runtime and
# handed to curl on stdin, never as an argument, never written anywhere.
# Only response bodies are saved; response headers go to `headers-dir`
# (default: a temp dir) for documentation and are not committed.
# About 25 requests, spaced out, well under TMDB's ~40/s.
set -euo pipefail

: "${TMDB_TOKEN_FILE:?set TMDB_TOKEN_FILE to a file containing a TMDB read access token}"
here="$(cd "$(dirname "$0")" && pwd)"
out="$here/fixtures/v3/recorded"
headers_dir="${1:-$(mktemp -d)}"
mkdir -p "$out" "$headers_dir"
api="https://api.themoviedb.org"

# fetch NAME PATH [TOKEN_SOURCE]: body to $out/NAME.json, status to stdout.
fetch() {
  local name="$1" path="$2" source="${3:-$TMDB_TOKEN_FILE}"
  local status
  status=$(printf 'Authorization: Bearer %s\n' "$(tr -d '\r\n' < "$source")" |
    curl -sS -m 20 -H @- -H 'Accept: application/json' \
      -D "$headers_dir/$name.headers" -o "$out/$name.json" -w '%{http_code}' "$api/$path")
  echo "$status $name"
  sleep 0.3
}

append_movie='append_to_response=external_ids,videos,release_dates,watch/providers,credits'
append_tv='append_to_response=external_ids,videos,content_ratings,watch/providers,aggregate_credits'

fetch authentication '3/authentication'
bogus="$(mktemp)"
printf 'not-a-valid-token' > "$bogus"
fetch error_invalid_key '3/authentication' "$bogus"
rm -f "$bogus"
fetch error_not_found '3/movie/999999999'
fetch configuration '3/configuration'
fetch configuration_languages '3/configuration/languages'
fetch configuration_countries '3/configuration/countries'
fetch watch_providers_regions '3/watch/providers/regions'
fetch watch_providers_movie_CH '3/watch/providers/movie?watch_region=CH'
fetch genre_movie_list '3/genre/movie/list?language=en-US'
fetch genre_tv_list '3/genre/tv/list?language=en-US'
fetch trending_movie_week_page1 '3/trending/movie/week?language=en-US&page=1'
fetch trending_movie_week_page2 '3/trending/movie/week?language=en-US&page=2'
fetch trending_tv_week_page1 '3/trending/tv/week?language=en-US&page=1'
fetch discover_movie_popular_page500 '3/discover/movie?language=en-US&page=500&include_adult=false&sort_by=popularity.desc'
fetch error_page_501 '3/discover/movie?language=en-US&page=501&include_adult=false&sort_by=popularity.desc'
fetch discover_tv_on_services_CH '3/discover/tv?language=en-US&page=1&include_adult=false&with_original_language=en%7Cde&without_genres=10767,10763,10766,10764&sort_by=popularity.desc&watch_region=CH&with_watch_providers=8%7C337&with_watch_monetization_types=flatrate%7Cfree%7Cads'
fetch discover_movie_top_rated '3/discover/movie?language=en-US&page=1&include_adult=false&sort_by=vote_average.desc&vote_count.gte=5000'
fetch search_multi_inception '3/search/multi?query=inception&page=1&language=en-US&include_adult=false'
fetch movie_27205 "3/movie/27205?language=en-US&$append_movie&include_video_language=en,null"
fetch tv_1399 "3/tv/1399?language=en-US&$append_tv&include_video_language=en,null"
# Spec 0008 slice 2: a season and "New episodes" of a category page.
fetch tv_1399_season_1 '3/tv/1399/season/1?language=en-US'
fetch tv_on_the_air '3/tv/on_the_air?language=en-US&page=1'
fetch movie_496243_de-DE "3/movie/496243?language=de-DE&$append_movie&include_video_language=de,en,null"
fetch movie_496243_en-US '3/movie/496243?language=en-US'
fetch tv_93405_de-CH '3/tv/93405?language=de-CH'
fetch tv_93405_de-DE "3/tv/93405?language=de-DE&$append_tv&include_video_language=de,en,null"
fetch tv_93405_en-US '3/tv/93405?language=en-US'

echo "headers in $headers_dir"
