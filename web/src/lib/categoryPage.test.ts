import { describe, expect, it } from 'vitest'

import type { ArtistDetails, DiscoverCategory, TitleCard } from '@/api/discover'

import {
  afterHero,
  discographyCards,
  gridFilters,
  isSeeAllList,
  listParams,
  splitFresh,
  todayIso,
} from './categoryPage'

const card = (id: string, extra: Partial<TitleCard> = {}): TitleCard => ({
  kind: 'movie',
  id,
  title: `T${id}`,
  status: 'new',
  requestedByMe: false,
  journeys: [],
  genres: [],
  ...extra,
})

const MOVIES: DiscoverCategory = {
  category: 'movies',
  kind: 'movie',
  discoverReady: true,
  requestable: true,
  lists: ['trending', 'popular', 'upcoming', 'top_rated'],
  providers: [{ id: 8, name: 'Netflix' }],
  genres: [{ id: '18', name: 'Drama' }],
  attributions: [],
}

const MUSIC: DiscoverCategory = {
  ...MOVIES,
  category: 'music',
  kind: 'album',
  lists: ['trending', 'fresh_releases', 'popular'],
  providers: [],
  genres: [{ id: 'rock', name: 'Rock' }],
}

describe('afterHero (spec 0008 F-3)', () => {
  const trending = Array.from({ length: 20 }, (_, i) => card(String(i + 1)))

  it('continues the hero: ranks 11–20 when the hero shows 1–10', () => {
    const hero = trending.slice(0, 10)
    const lane = afterHero(trending, hero)
    expect(lane.map((r) => r.rank)).toEqual([11, 12, 13, 14, 15, 16, 17, 18, 19, 20])
    expect(lane[0]?.card.id).toBe('11')
  })

  it('keeps a title the hero skipped (no backdrop) at its own rank, at most ten', () => {
    // The hero left out rank 4 and took rank 11 instead.
    const hero = [...trending.slice(0, 3), ...trending.slice(4, 11)]
    const lane = afterHero(trending, hero)
    expect(lane.map((r) => r.rank)).toEqual([4, 12, 13, 14, 15, 16, 17, 18, 19, 20])
  })

  it('starts at 1 without a hero (music, or a hero that failed)', () => {
    const lane = afterHero(trending, [])
    expect(lane.map((r) => r.rank)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
  })

  it('tells titles apart by kind and id', () => {
    const lane = afterHero([card('1'), card('1', { kind: 'series' })], [card('1')])
    expect(lane).toHaveLength(1)
    expect(lane[0]?.card.kind).toBe('series')
  })
})

describe('splitFresh', () => {
  it('puts future releases into "Out soon", soonest first, the rest into fresh releases', () => {
    const cards = [
      card('a', { releaseDate: '2026-11-20' }),
      card('b', { releaseDate: '2026-11-09' }),
      card('c', { releaseDate: '2026-11-07' }),
      card('d', { releaseDate: '2026-11-01' }),
      card('e'),
    ]
    const { outSoon, fresh } = splitFresh(cards, '2026-11-07')
    expect(outSoon.map((c) => c.id)).toEqual(['b', 'a'])
    expect(fresh.map((c) => c.id)).toEqual(['c', 'd', 'e'])
  })

  it('formats today as a calendar day', () => {
    expect(todayIso(new Date(2026, 10, 7, 23, 30))).toBe('2026-11-07')
  })
})

describe('discographyCards', () => {
  it('turns the albums in the default view into cards with the artist and journeys', () => {
    const artist = {
      id: 'ar',
      name: 'Juno Lark',
      albums: [
        {
          id: 'al-1',
          title: 'First',
          year: 2026,
          cover: 'al-1',
          inDefaultView: true,
          requestedByMe: true,
          secondaryTypes: [],
          status: 'requested',
        },
        {
          id: 'al-2',
          title: 'Live',
          inDefaultView: false,
          requestedByMe: false,
          secondaryTypes: ['live'],
          status: 'new',
        },
      ],
      journeys: [{ album: 'al-1', journey: { steps: [] } }],
    } as unknown as ArtistDetails
    const cards = discographyCards(artist)
    expect(cards).toHaveLength(1)
    expect(cards[0]).toMatchObject({
      kind: 'album',
      id: 'al-1',
      title: 'First',
      artist: 'Juno Lark',
      poster: 'al-1',
      status: 'requested',
      requestedByMe: true,
    })
    expect(cards[0]?.journeys).toHaveLength(1)
  })
})

describe('See all lists and filters (spec 0008 Q6 B)', () => {
  it('knows which lists have a grid', () => {
    expect(isSeeAllList(MOVIES, 'popular')).toBe(true)
    expect(isSeeAllList(MOVIES, 'on_provider')).toBe(true)
    expect(isSeeAllList(MOVIES, 'genre')).toBe(true)
    expect(isSeeAllList(MOVIES, 'on_the_air')).toBe(false)
    expect(isSeeAllList({ ...MOVIES, category: 'series', kind: 'series' }, 'on_the_air')).toBe(true)
    expect(isSeeAllList({ ...MOVIES, providers: [] }, 'on_provider')).toBe(false)
    expect(isSeeAllList(MOVIES, 'fresh_releases')).toBe(false)
    expect(isSeeAllList(MOVIES, 'nonsense')).toBe(false)
    expect(isSeeAllList(MUSIC, 'genre')).toBe(true)
    expect(isSeeAllList(MUSIC, 'on_provider')).toBe(false)
  })

  it('offers provider, genre and sort only where the catalog takes them', () => {
    expect(gridFilters(MOVIES, 'popular')).toEqual({ provider: true, genre: true, sort: true })
    expect(gridFilters(MOVIES, 'on_provider')).toEqual({ provider: true, genre: true, sort: true })
    expect(gridFilters(MOVIES, 'trending')).toEqual({ provider: false, genre: false, sort: false })
    expect(gridFilters(MOVIES, 'upcoming')).toEqual({ provider: false, genre: false, sort: false })
    expect(gridFilters({ ...MOVIES, providers: [] }, 'popular')).toEqual({
      provider: false,
      genre: true,
      sort: true,
    })
    expect(gridFilters(MUSIC, 'genre')).toEqual({ provider: false, genre: true, sort: false })
    expect(gridFilters(MUSIC, 'trending')).toEqual({ provider: false, genre: false, sort: false })
  })

  it('sends only what is set, and the sort only when it differs from the list', () => {
    expect(
      listParams('popular', { provider: 0, genre: '', sort: 'popularity', hideOwned: false }),
    ).toEqual({})
    expect(
      listParams('top_rated', { provider: 8, genre: '18', sort: 'rating', hideOwned: true }),
    ).toEqual({ provider: 8, genre: '18', hideOwned: true })
    expect(
      listParams('popular', { provider: 0, genre: '', sort: 'release_date', hideOwned: false }),
    ).toEqual({ sort: 'release_date' })
  })
})
