import { describe, expect, it } from 'vitest'

import { uniqueCards, type TitleCard } from './discover'

function card(kind: TitleCard['kind'], id: number): TitleCard {
  return {
    kind,
    id: String(id),
    title: `${kind} ${id}`,
    status: 'new',
    requestedByMe: false,
    journeys: [],
    genres: [],
  }
}

describe('uniqueCards', () => {
  it('keeps each title once across pages, movie and series apart', () => {
    const pages = [
      { items: [card('movie', 1), card('movie', 2)], next: 2 },
      { items: [card('movie', 2), card('series', 2), card('movie', 3)], next: null },
    ]
    expect(uniqueCards(pages).map((c) => `${c.kind}-${c.id}`)).toEqual([
      'movie-1',
      'movie-2',
      'series-2',
      'movie-3',
    ])
    expect(uniqueCards(undefined)).toEqual([])
  })
})
