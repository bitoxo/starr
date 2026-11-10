import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { retryUnlessNotFound } from '@/lib/detail'

import { unwrap } from './errors'
import { FOLLOWS } from './follows'
import type { components } from './schema'

export type DiscoverCategory = components['schemas']['DiscoverCategory']
export type DiscoverList = components['schemas']['DiscoverList']
export type DiscoverGenre = components['schemas']['DiscoverGenre']
export type Attribution = components['schemas']['Attribution']
export type TitleCard = components['schemas']['TitleCard']
export type Highlight = components['schemas']['Highlight']
export type TitlePage = components['schemas']['TitlePage']
export type TitleDetails = components['schemas']['TitleDetails']
export type SeasonDetails = components['schemas']['SeasonDetails']
export type Availability = components['schemas']['Availability']
export type MediaKind = components['schemas']['MediaKind']
export type MyRequest = components['schemas']['MyRequest']
export type CreatedRequest = components['schemas']['CreatedRequest']
export type SkippedUnit = components['schemas']['SkippedUnit']
export type ArtistDetails = components['schemas']['ArtistDetails']
export type AlbumDetails = components['schemas']['AlbumDetails']
export type DiscographyAlbum = components['schemas']['DiscographyAlbum']
export type ReleaseType = components['schemas']['ReleaseType']
export type SecondaryType = components['schemas']['SecondaryType']
export type SeasonEpisodes = components['schemas']['SeasonEpisodes']

/**
 * A title as the API addresses it (ADR 0012): its kind and its id in the category's catalog,
 * an opaque string (the TMDB id for movies and series).
 */
export interface TitleRef {
  kind: MediaKind
  id: string
}

const DISCOVER = ['discover'] as const
const TITLES = ['titles'] as const
const MINE = ['requests', 'mine'] as const

export function useDiscoverCategories() {
  return useQuery({
    queryKey: [...DISCOVER, 'categories'],
    queryFn: () => unwrap(api.GET('/api/v1/discover')),
  })
}

const nextPage = (last: TitlePage) => last.next ?? undefined

/**
 * The cards of all loaded pages, each title once: catalog lists shift between page loads, so
 * a title can show up on page n and again on n+1.
 */
export function uniqueCards(pages: TitlePage[] | undefined): TitleCard[] {
  const seen = new Set<string>()
  return (pages ?? [])
    .flatMap((p) => p.items)
    .filter((card) => {
      const key = `${card.kind}-${card.id}`
      if (seen.has(key)) return false
      seen.add(key)
      return true
    })
}

/**
 * A list the category pages ask for (spec 0008 slice 2): the Discover lists, plus "On
 * \<provider\>" (`provider`), one genre (`genre`) and, for series, "On the air".
 */
export type PageList = DiscoverList | 'on_provider' | 'genre' | 'on_the_air'

export type ListSort = 'popularity' | 'rating' | 'release_date'

/** Filters of a list; unset ones are left out of the request. */
export interface ListParams {
  provider?: number
  genre?: string
  sort?: ListSort
  hideOwned?: boolean
}

/** One list of a category, page by page for endless scrolling. */
export function useDiscoverList(
  category: string,
  list: PageList,
  params: ListParams = {},
  { enabled = true }: { enabled?: boolean } = {},
) {
  return useInfiniteQuery({
    queryKey: [...DISCOVER, 'list', category, list, params],
    enabled,
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET('/api/v1/discover/{category}/{list}', {
          params: { path: { category, list }, query: { page: pageParam, ...params } },
        }),
      ),
    initialPageParam: 1,
    getNextPageParam: nextPage,
  })
}

/**
 * The hero (spec 0008 A4, Q2 A): up to ten trending movies and series with a backdrop,
 * interleaved by rank, or one category's top 10 on its page (`category`). Under `discover`, so
 * a request refreshes it like the lanes. Music has none (no backdrops): `enabled` false.
 */
export function useHighlights(category?: string, { enabled = true }: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: [...DISCOVER, 'highlights', category ?? null],
    enabled,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/discover/highlights', {
          params: { query: category ? { category } : {} },
        }),
      ),
  })
}

/** Search across movies, series, artists and albums; idle while `query` is blank. */
export function useSearch(query: string) {
  const text = query.trim()
  return useInfiniteQuery({
    queryKey: [...DISCOVER, 'search', text],
    enabled: text !== '',
    queryFn: ({ pageParam }) =>
      unwrap(api.GET('/api/v1/search', { params: { query: { query: text, page: pageParam } } })),
    initialPageParam: 1,
    getNextPageParam: nextPage,
  })
}

/**
 * A title's details. `staleTime` for callers that only show facts and pass by often (the hero
 * revisits its ten titles every round), so they do not ask again each time.
 */
export function useTitle(ref: TitleRef | null, { staleTime }: { staleTime?: number } = {}) {
  return useQuery({
    queryKey: [...TITLES, ref?.kind, ref?.id],
    enabled: ref !== null,
    staleTime,
    // An unknown title says "not found" at once instead of retrying (MEM-32).
    retry: retryUnlessNotFound,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/titles/{kind}/{id}', {
          params: { path: { kind: ref?.kind ?? 'movie', id: ref?.id ?? '' } },
        }),
      ),
  })
}

/**
 * The episodes of one season, asked for when its "Episodes" disclosure opens (spec 0008 A7).
 * Kept for a day like the server's cache; not under `titles`, so a request does not reload it.
 */
export function useSeasonEpisodes(seriesId: string, season: number, enabled: boolean) {
  return useQuery({
    queryKey: ['seasons', seriesId, season],
    enabled,
    staleTime: 24 * 60 * 60 * 1000,
    retry: retryUnlessNotFound,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/titles/series/{id}/seasons/{number}', {
          params: { path: { id: seriesId, number: season } },
        }),
      ),
  })
}

/** An artist with their discography and each album's status. */
export function useArtist(id: string | null) {
  return useQuery({
    queryKey: [...TITLES, 'artist', id],
    enabled: id !== null,
    retry: retryUnlessNotFound,
    queryFn: () => unwrap(api.GET('/api/v1/artists/{id}', { params: { path: { id: id ?? '' } } })),
  })
}

/** An album with its status and the member's journey. */
export function useAlbum(id: string | null, { staleTime }: { staleTime?: number } = {}) {
  return useQuery({
    queryKey: [...TITLES, 'album', id],
    enabled: id !== null,
    staleTime,
    retry: retryUnlessNotFound,
    queryFn: () => unwrap(api.GET('/api/v1/albums/{id}', { params: { path: { id: id ?? '' } } })),
  })
}

/** Details are only asked for their "Open in" link here; covers are passed by often. */
const OPEN_IN_STALE = 10 * 60_000

/**
 * Where an available title opens: the first media server link of its details (movies and
 * series) or album. Asked only while `enabled` (the title is available), so browsing a lane
 * does not load the details of every cover.
 */
export function useOpenIn(card: Pick<TitleCard, 'kind' | 'id'>, enabled: boolean) {
  const video = card.kind === 'movie' || card.kind === 'series'
  const title = useTitle(enabled && video ? { kind: card.kind, id: card.id } : null, {
    staleTime: OPEN_IN_STALE,
  })
  const album = useAlbum(enabled && card.kind === 'album' ? card.id : null, {
    staleTime: OPEN_IN_STALE,
  })
  const query = video ? title : album
  return {
    link: enabled ? query.data?.openIn[0] : undefined,
    pending: enabled && (video || card.kind === 'album') && query.isPending,
  }
}

/** The member's requests; with `showHidden` also the ones they hid. */
export function useMyRequests(showHidden = false) {
  return useQuery({
    queryKey: [...MINE, { hidden: showHidden }],
    queryFn: () =>
      unwrap(api.GET('/api/v1/requests', { params: { query: { hidden: showHidden } } })),
    // Progress moves while downloads run; polling stops in background tabs.
    refetchInterval: 30_000,
  })
}

/** A request changes what lists, search, details and "Requests" show. */
function useMemberRequestMutation<V, R>(call: (vars: V) => Promise<R>) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: call,
    onSettled: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: DISCOVER }),
        client.invalidateQueries({ queryKey: TITLES }),
        client.invalidateQueries({ queryKey: MINE }),
        // Requesting with "also get new seasons/albums" follows the series or artist.
        client.invalidateQueries({ queryKey: FOLLOWS }),
        // Withdraw, cancel and hide also act on the private area's requests.
        client.invalidateQueries({ queryKey: ['private'] }),
      ]),
  })
}

export interface NewRequest {
  kind: MediaKind
  titleId: string
  /** Series only. */
  seasons?: number[]
  /** Artists only: the albums (release-group ids) to request. */
  albums?: string[]
  follow?: boolean
}

export function useCreateRequest() {
  return useMemberRequestMutation((body: NewRequest) =>
    unwrap(api.POST('/api/v1/requests', { body })),
  )
}

export function useWithdrawRequest() {
  return useMemberRequestMutation((id: number) =>
    unwrap(api.POST('/api/v1/requests/{id}/withdraw', { params: { path: { id } } })),
  )
}

/** Cancel what a request still asks for and has not started downloading (spec 0004). */
export function useCancelRequest() {
  return useMemberRequestMutation((id: number) =>
    unwrap(api.POST('/api/v1/requests/{id}/cancel', { params: { path: { id } } })),
  )
}

/** Hide a finished request from the member's list, or show it again. */
export function useHideRequest() {
  return useMemberRequestMutation(({ id, hidden }: { id: number; hidden: boolean }) =>
    unwrap(api.PUT('/api/v1/requests/{id}/hidden', { params: { path: { id } }, body: { hidden } })),
  )
}
