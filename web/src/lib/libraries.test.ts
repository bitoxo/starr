import { describe, expect, it } from 'vitest'

import { libraryFits } from './libraries'

describe('libraryFits (ADM-19)', () => {
  it('flags a library whose content does not match the category', () => {
    expect(libraryFits('movies', 'books')).toBe(false)
    expect(libraryFits('movies', 'series')).toBe(false)
    expect(libraryFits('music', 'movies')).toBe(false)
    expect(libraryFits('adult', 'music')).toBe(false)
  })

  it('accepts matching and mixed libraries', () => {
    expect(libraryFits('movies', 'movies')).toBe(true)
    expect(libraryFits('series', 'series')).toBe(true)
    expect(libraryFits('music', 'music')).toBe(true)
    expect(libraryFits('adult', 'movies')).toBe(true)
    expect(libraryFits('movies', 'other')).toBe(true)
  })
})
