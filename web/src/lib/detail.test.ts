import { describe, expect, it } from 'vitest'

import type { AlbumDetails, MyRequest, TitleDetails } from '@/api/discover'
import { ApiError } from '@/api/errors'

import {
  crewGroups,
  discs,
  isNotFound,
  providerGroups,
  retryUnlessNotFound,
  seriesEnded,
  withdrawables,
} from './detail'

type Provider = TitleDetails['watchProviders'][number]
type Track = AlbumDetails['tracks'][number]

const provider = (id: number, kind: Provider['kind'], name = `P${id}`): Provider => ({
  id,
  kind,
  name,
  region: 'DE',
  logo: `${id}.png`,
})

describe('providerGroups', () => {
  it('puts flatrate, free and ads under stream, then rent and buy, each once', () => {
    const groups = providerGroups([
      provider(8, 'flatrate', 'Netflix'),
      provider(2, 'rent', 'Apple TV'),
      provider(3, 'buy', 'Google Play'),
      provider(2, 'buy', 'Apple TV'),
      provider(9, 'ads', 'Pluto'),
      provider(8, 'free', 'Netflix'),
    ])
    expect(groups.map(([kind, list]) => [kind, list.map((p) => p.name)])).toEqual([
      ['stream', ['Netflix', 'Pluto']],
      ['rent', ['Apple TV']],
      ['buy', ['Google Play', 'Apple TV']],
    ])
  })

  it('drops offers without a name (nothing to show for them)', () => {
    expect(providerGroups([provider(2, 'rent', ''), provider(3, 'rent', 'Sky')])).toEqual([
      ['rent', [provider(3, 'rent', 'Sky')]],
    ])
  })

  it('leaves out empty groups', () => {
    expect(providerGroups([provider(2, 'rent')]).map(([kind]) => kind)).toEqual(['rent'])
    expect(providerGroups([])).toEqual([])
  })
})

describe('seriesEnded', () => {
  it('knows TMDB’s finished statuses', () => {
    expect(seriesEnded('Ended')).toBe(true)
    expect(seriesEnded('Canceled')).toBe(true)
    expect(seriesEnded('Returning Series')).toBe(false)
    expect(seriesEnded('In Production')).toBe(false)
    expect(seriesEnded(null)).toBe(false)
    expect(seriesEnded(undefined)).toBe(false)
  })
})

describe('crewGroups', () => {
  it('splits leads (directors, creators) from writers, in their order', () => {
    expect(
      crewGroups([
        { name: 'Ann', role: 'writer' },
        { name: 'Bo', role: 'director' },
        { name: 'Cy', role: 'creator' },
      ]),
    ).toEqual({
      leads: [
        { name: 'Bo', role: 'director' },
        { name: 'Cy', role: 'creator' },
      ],
      writers: [{ name: 'Ann', role: 'writer' }],
    })
  })
})

const track = (disc: number, position: number, lengthSeconds: number | null, discTitle = '') =>
  ({ disc, position, title: `T${disc}.${position}`, discTitle, lengthSeconds }) as Track

describe('discs', () => {
  it('groups tracks by disc with their running time and the total', () => {
    const result = discs([track(1, 1, 200), track(1, 2, 100), track(2, 1, 60, 'Bonus')])
    expect(result.discs.map((d) => [d.disc, d.title, d.tracks.length, d.seconds])).toEqual([
      [1, '', 2, 300],
      [2, 'Bonus', 1, 60],
    ])
    expect(result.totalSeconds).toBe(360)
  })

  it('has no total while any length is unknown', () => {
    expect(discs([track(1, 1, 200), track(1, 2, null)]).totalSeconds).toBeNull()
    expect(discs([]).totalSeconds).toBeNull()
  })
})

function request(patch: Partial<MyRequest>): MyRequest {
  return {
    id: 1,
    kind: 'movie',
    titleId: '27205',
    title: 'Inception',
    category: 'movies',
    state: 'pending_approval',
    follow: false,
    canWithdraw: true,
    canCancel: true,
    canHide: false,
    hidden: false,
    createdAt: '2026-11-01T12:00:00Z',
    updatedAt: '2026-11-01T12:00:00Z',
    openIn: [],
    items: [{ state: 'pending_approval', journey: { steps: [] } }],
    ...patch,
  }
}

describe('withdrawables', () => {
  it('finds the member’s pending requests for a movie or series', () => {
    const requests = [
      request({ id: 1 }),
      request({ id: 2, titleId: '1' }),
      request({ id: 3, kind: 'series', titleId: '27205' }),
    ]
    expect(withdrawables(requests, { kind: 'movie', id: '27205' }).map((w) => w.id)).toEqual([1])
  })

  it('skips requests with anything beyond waiting for approval', () => {
    const requests = [
      request({ id: 1, canWithdraw: false }),
      request({
        id: 2,
        items: [
          { state: 'pending_approval', journey: { steps: [] } },
          { state: 'approved', journey: { steps: [] } },
        ],
      }),
    ]
    expect(withdrawables(requests, { kind: 'movie', id: '27205' })).toEqual([])
  })

  it('names the seasons of a series request', () => {
    const series = request({
      kind: 'series',
      titleId: '1399',
      items: [
        { season: 3, state: 'pending_approval', journey: { steps: [] } },
        { season: 4, state: 'pending_approval', journey: { steps: [] } },
      ],
    })
    expect(withdrawables([series], { kind: 'series', id: '1399' })).toEqual([
      { id: 1, seasons: [3, 4], albums: [] },
    ])
  })

  it('finds an album in its artist’s request', () => {
    const music = request({
      kind: 'artist',
      titleId: 'radiohead',
      items: [
        {
          album: { id: 'okc', title: 'OK Computer' },
          state: 'pending_approval',
          journey: { steps: [] },
        },
      ],
    })
    expect(withdrawables([music], { kind: 'album', id: 'okc' })).toEqual([
      { id: 1, seasons: [], albums: ['OK Computer'] },
    ])
    expect(withdrawables([music], { kind: 'album', id: 'other' })).toEqual([])
    expect(withdrawables([music], { kind: 'artist', id: 'radiohead' })).toHaveLength(1)
  })
})

describe('not found', () => {
  it('tells a 404 apart and never retries it (MEM-32)', () => {
    const missing = new ApiError('catalog.not_found', 404)
    const down = new ApiError('unavailable', 502)
    expect(isNotFound(missing)).toBe(true)
    expect(isNotFound(down)).toBe(false)
    expect(isNotFound(new Error('x'))).toBe(false)
    expect(retryUnlessNotFound(0, missing)).toBe(false)
    expect(retryUnlessNotFound(0, down)).toBe(true)
    expect(retryUnlessNotFound(3, down)).toBe(false)
  })
})
