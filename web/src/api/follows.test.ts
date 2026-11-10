import { describe, expect, it } from 'vitest'

import { type Follow, followCutoff, followOf } from './follows'

function follow(patch: Partial<Follow> = {}): Follow {
  return {
    id: 1,
    kind: 'series',
    titleId: '1399',
    title: 'Game of Thrones',
    year: 2011,
    poster: null,
    category: 'series',
    seasonsThrough: 2,
    createdAt: '2026-11-01T12:00:00Z',
    checkedAt: null,
    ...patch,
  }
}

describe('follows', () => {
  it('finds the follow of a title by kind and TMDB id', () => {
    const follows = [follow(), follow({ id: 2, titleId: '1396', title: 'Breaking Bad' })]
    expect(followOf(follows, { kind: 'series', id: '1396' })?.id).toBe(2)
    expect(followOf(follows, { kind: 'movie', id: '1399' })).toBeUndefined()
    expect(followOf(undefined, { kind: 'series', id: '1399' })).toBeUndefined()
  })

  it('names the cutoff only once a season had an air date', () => {
    expect(followCutoff(follow())).toBe(2)
    expect(followCutoff(follow({ seasonsThrough: 0 }))).toBeNull()
  })

  it('has no seasons cutoff for an artist, whose cutoff is a day', () => {
    const artist = follow({
      kind: 'artist',
      titleId: 'a74b1b7f-71a5-4011-9441-d0b5e4122711',
      category: 'music',
      seasonsThrough: null,
      knownThrough: '2026-11-03',
    })
    expect(followCutoff(artist)).toBeNull()
    expect(
      followOf([follow(), artist], { kind: 'artist', id: 'a74b1b7f-71a5-4011-9441-d0b5e4122711' })
        ?.kind,
    ).toBe('artist')
  })
})
