import type {
  ArtistDetails,
  DiscoverCategory,
  ListParams,
  ListSort,
  PageList,
  TitleCard,
} from '@/api/discover'

/*
 * The data rules of the category pages and their "See all" grid (spec 0008 slice 4, Q6 A + B).
 * Pure functions, so the page components only lay things out.
 */

/** Titles a ranked lane shows. */
export const RANKED = 10

const keyOf = (card: { kind: string; id: string }) => `${card.kind}-${card.id}`

/**
 * The ranked "More trending" lane (F-3): the hero shows the category's top 10, the lane
 * continues with the next ten of the Trending list, each at its rank there. A title the hero
 * left out (it had no backdrop) keeps its own rank in the lane. Without a hero (music, or a
 * hero that failed) the lane starts at 1.
 */
export function afterHero(
  trending: TitleCard[],
  hero: { kind: string; id: string }[],
): { card: TitleCard; rank: number }[] {
  const shown = new Set(hero.map(keyOf))
  return trending
    .map((card, i) => ({ card, rank: i + 1 }))
    .filter(({ card }) => !shown.has(keyOf(card)))
    .slice(0, RANKED)
}

/** Today as a calendar day (`2026-11-07`) in the browser's time zone. */
export function todayIso(now: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`
}

/**
 * Music's fresh releases split at today: "Out soon" (released after today, soonest first) and
 * the fresh releases (today and before, in the list's order, undated last as they came).
 */
export function splitFresh(
  cards: TitleCard[],
  today: string,
): { outSoon: TitleCard[]; fresh: TitleCard[] } {
  const outSoon = cards
    .filter((c) => c.releaseDate != null && c.releaseDate > today)
    .sort((a, b) => (a.releaseDate ?? '').localeCompare(b.releaseDate ?? ''))
  const fresh = cards.filter((c) => !(c.releaseDate != null && c.releaseDate > today))
  return { outSoon, fresh }
}

/** An artist's albums of the default view (newest first) as lane cards. */
export function discographyCards(artist: ArtistDetails): TitleCard[] {
  return artist.albums
    .filter((album) => album.inDefaultView)
    .map((album) => ({
      kind: 'album',
      id: album.id,
      title: album.title,
      year: album.year,
      poster: album.cover,
      releaseDate: album.releaseDate,
      releaseType: album.releaseType,
      artist: artist.name,
      status: album.status,
      requestedByMe: album.requestedByMe,
      journeys: artist.journeys.filter((j) => j.album === album.id),
      genres: [],
    }))
}

/** Lists a TMDB catalog sorts and filters by provider and genre (the grid's full toolbar). */
const BROWSE: PageList[] = ['popular', 'top_rated', 'on_provider', 'genre']

const isTmdb = (category: DiscoverCategory) =>
  category.kind === 'movie' || category.kind === 'series'

/** Whether `/discover/:category/:list` has a grid for this category. */
export function isSeeAllList(category: DiscoverCategory, list: string): list is PageList {
  if (list === 'on_provider') return isTmdb(category) && category.providers.length > 0
  if (list === 'genre') return category.genres.length > 0
  if (list === 'on_the_air') return category.kind === 'series'
  return (category.lists as string[]).includes(list)
}

/** The grid's filters for a list: what the catalog can answer (spec 0008 slice 2). */
export function gridFilters(
  category: DiscoverCategory,
  list: PageList,
): { provider: boolean; genre: boolean; sort: boolean } {
  if (!isTmdb(category)) {
    return { provider: false, genre: list === 'genre', sort: false }
  }
  const browse = BROWSE.includes(list)
  return {
    provider: browse && category.providers.length > 0,
    genre: browse && category.genres.length > 0,
    sort: browse,
  }
}

/** The sort a list has by itself. */
export function defaultSort(list: PageList): ListSort {
  return list === 'top_rated' ? 'rating' : 'popularity'
}

/** The grid's choices as list parameters: unset ones and the list's own sort left out. */
export function listParams(
  list: PageList,
  chosen: { provider: number; genre: string; sort: ListSort; hideOwned: boolean },
): ListParams {
  const params: ListParams = {}
  if (chosen.provider > 0) params.provider = chosen.provider
  if (chosen.genre !== '') params.genre = chosen.genre
  if (chosen.sort !== defaultSort(list)) params.sort = chosen.sort
  if (chosen.hideOwned) params.hideOwned = true
  return params
}
