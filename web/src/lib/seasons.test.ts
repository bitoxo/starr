import { describe, expect, it } from 'vitest'

import type { SeasonDetails } from '@/api/discover'

import { seasonBlock, selectableSeasons, toggleAll, titleRefParam, parseTitleRef } from './seasons'

function season(number: number, patch: Partial<SeasonDetails> = {}): SeasonDetails {
  return {
    number,
    name: number === 0 ? 'Specials' : `Season ${number}`,
    episodeCount: 10,
    airDate: '2020-01-01',
    special: number === 0,
    status: 'new',
    requestedByMe: false,
    ...patch,
  }
}

describe('season selection', () => {
  it('blocks seasons that are available or that I asked for already', () => {
    expect(seasonBlock(season(1, { status: 'available' }))).toBe('available')
    expect(seasonBlock(season(1, { status: 'requested', requestedByMe: true }))).toBe('mine')
    // Requested by someone else: I can still join (D10).
    expect(seasonBlock(season(1, { status: 'requested' }))).toBeNull()
    // Partly there: the rest can still be requested.
    expect(seasonBlock(season(1, { status: 'partially_available' }))).toBeNull()
  })

  it('"select all" takes the open regular seasons, not specials', () => {
    const seasons = [
      season(0),
      season(1, { status: 'available' }),
      season(2),
      season(3, { requestedByMe: true, status: 'requested' }),
      season(4),
    ]
    expect(selectableSeasons(seasons)).toEqual([2, 4])
    expect(toggleAll(seasons, new Set())).toEqual(new Set([2, 4]))
    expect(toggleAll(seasons, new Set([2, 4]))).toEqual(new Set())
    // Partly selected: select the rest, keep a chosen special.
    expect(toggleAll(seasons, new Set([0, 2]))).toEqual(new Set([0, 2, 4]))
  })
})

describe('title reference in the URL', () => {
  it('round-trips and rejects anything else', () => {
    expect(titleRefParam({ kind: 'series', id: '1399' })).toBe('series-1399')
    expect(parseTitleRef('series-1399')).toEqual({ kind: 'series', id: '1399' })
    expect(parseTitleRef('movie-27205')).toEqual({ kind: 'movie', id: '27205' })
    // Opaque catalog ids: the first dash ends the kind.
    const mbid = 'a74b1b7f-71a5-4011-9441-d0b5e4122711'
    expect(titleRefParam({ kind: 'series', id: mbid })).toBe(`series-${mbid}`)
    expect(parseTitleRef(`series-${mbid}`)).toEqual({ kind: 'series', id: mbid })
    expect(parseTitleRef('series-')).toBeNull()
    expect(parseTitleRef('series-<script>')).toBeNull()
    expect(parseTitleRef(`album-${mbid}`)).toEqual({ kind: 'album', id: mbid })
    expect(parseTitleRef(`artist-${mbid}`)).toEqual({ kind: 'artist', id: mbid })
    expect(parseTitleRef('person-1')).toBeNull()
    expect(parseTitleRef('movie-')).toBeNull()
    expect(parseTitleRef(null)).toBeNull()
  })
})
