import { describe, expect, it } from 'vitest'

import type { DiscographyAlbum, TitleCard } from '@/api/discover'

import { discographyGroups, groupOf, searchGroups } from './music'

function album(patch: Partial<DiscographyAlbum>): DiscographyAlbum {
  return {
    id: 'x',
    title: 'x',
    year: null,
    releaseDate: null,
    releaseType: 'album',
    secondaryTypes: [],
    cover: null,
    inDefaultView: true,
    status: 'new',
    requestedByMe: false,
    ...patch,
  }
}

function card(kind: TitleCard['kind'], id: string): TitleCard {
  return {
    kind,
    id,
    title: id,
    year: null,
    poster: null,
    status: 'new',
    requestedByMe: false,
    journeys: [],
    genres: [],
  }
}

describe('music helpers', () => {
  it('groups secondary types together, whatever the primary type', () => {
    expect(groupOf(album({}))).toBe('album')
    expect(groupOf(album({ releaseType: 'ep' }))).toBe('ep')
    expect(groupOf(album({ releaseType: 'single' }))).toBe('single')
    expect(groupOf(album({ releaseType: 'broadcast' }))).toBe('other')
    expect(groupOf(album({ releaseType: null }))).toBe('other')
    expect(groupOf(album({ secondaryTypes: ['live'] }))).toBe('secondary')
  })

  it('shows only the default view until "show all", groups in a fixed order', () => {
    const albums = [
      album({ id: 'live', secondaryTypes: ['live'], inDefaultView: false }),
      album({ id: 'ep', releaseType: 'ep' }),
      album({ id: 'lp' }),
    ]
    expect(discographyGroups(albums, false).map(([g]) => g)).toEqual(['album', 'ep'])
    expect(discographyGroups(albums, true).map(([g]) => g)).toEqual(['album', 'ep', 'secondary'])
  })

  it('splits search results into movies and series, artists, albums', () => {
    const groups = searchGroups([
      card('album', 'a'),
      card('movie', 'm'),
      card('artist', 'r'),
      card('series', 's'),
    ])
    expect(groups.map(([g, items]) => [g, items.map((c) => c.id)])).toEqual([
      ['video', ['m', 's']],
      ['artist', ['r']],
      ['album', ['a']],
    ])
    expect(searchGroups([card('movie', 'm')]).map(([g]) => g)).toEqual(['video'])
  })
})
