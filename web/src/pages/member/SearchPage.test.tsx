import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { useLocation } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { DiscoverCategory, TitleCard } from '@/api/discover'
import { ok, renderWithProviders } from '@/test/render'

import SearchPage from './SearchPage'

const api = vi.hoisted(() => ({ GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const category = (name: 'movies' | 'series' | 'music'): DiscoverCategory => ({
  category: name,
  kind: name === 'movies' ? 'movie' : name === 'series' ? 'series' : 'album',
  discoverReady: true,
  requestable: true,
  lists: ['trending'],
  providers: [],
  genres: [],
  attributions: [],
})

const card = (kind: TitleCard['kind'], id: string, title: string, year: number): TitleCard => ({
  kind,
  id,
  title,
  year,
  poster: null,
  status: 'new',
  requestedByMe: false,
  journeys: [],
  genres: [],
})

const RESULTS = [
  card('movie', '1', 'Harbor Lights', 2001),
  card('series', '2', 'Harbor Watch', 2019),
  card('album', 'a', 'Harbor Songs', 2022),
]

function serve(items: TitleCard[] = RESULTS) {
  api.GET.mockImplementation((path: string) =>
    Promise.resolve(
      path === '/api/v1/discover'
        ? ok([category('movies'), category('series'), category('music')])
        : ok({ items, next: null }),
    ),
  )
}

function Search() {
  return <output data-testid="search">{useLocation().search}</output>
}

function renderAt(path: string) {
  return renderWithProviders(
    <>
      <SearchPage />
      <Search />
    </>,
    { path },
  )
}

describe('Search (spec 0008 IA, MEM-17)', () => {
  beforeEach(() => api.GET.mockReset())

  it('marks every result with its category, keeping the caption in the grid', async () => {
    serve()
    renderAt('/search?q=harbor')
    const movie = await screen.findByRole('button', { name: /Harbor Lights/ })
    expect(within(movie).getByText('Movie')).toBeTruthy()
    expect(within(movie).getByText('2001')).toBeTruthy()
    expect(
      within(screen.getByRole('button', { name: /Harbor Watch/ })).getByText('Series'),
    ).toBeTruthy()
    expect(
      within(screen.getByRole('button', { name: /Harbor Songs/ })).getByText('Album'),
    ).toBeTruthy()
  })

  it('filters by category through the URL', async () => {
    serve()
    renderAt('/search?q=harbor&category=series')
    const filters = await screen.findByRole('group', { name: 'Show results from' })
    expect(
      within(filters).getByRole('button', { name: 'Series' }).getAttribute('aria-pressed'),
    ).toBe('true')
    expect(await screen.findByRole('button', { name: /Harbor Watch/ })).toBeTruthy()
    expect(screen.queryByRole('button', { name: /Harbor Lights/ })).toBeNull()
    fireEvent.click(within(filters).getByRole('button', { name: 'Music' }))
    expect(screen.getByTestId('search').textContent).toBe('?q=harbor&category=music')
    expect(screen.getByRole('button', { name: /Harbor Songs/ })).toBeTruthy()
    fireEvent.click(within(filters).getByRole('button', { name: 'All' }))
    expect(screen.getByTestId('search').textContent).toBe('?q=harbor')
  })

  it('says when only the chosen category is empty, with a way to all results', async () => {
    serve([card('movie', '1', 'Harbor Lights', 2001)])
    renderAt('/search?q=harbor&category=music')
    expect(await screen.findByText('No music for “harbor”')).toBeTruthy()
    const showAll = screen.getByRole('button', { name: 'Show all results' })
    showAll.focus()
    fireEvent.click(showAll)
    expect(screen.getByTestId('search').textContent).toBe('?q=harbor')
    expect(await screen.findByRole('button', { name: /Harbor Lights/ })).toBeTruthy()
    // The button went away with the empty state: focus is on the "All" filter, not <body>.
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('button', { name: 'All' })),
    )
  })

  it('does not page through the whole search for an empty filter by itself', async () => {
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(
        path === '/api/v1/discover'
          ? ok([category('movies'), category('music')])
          : ok({ items: [card('movie', '1', 'Harbor Lights', 2001)], next: 2 }),
      ),
    )
    renderAt('/search?q=harbor&category=music')
    expect(
      await screen.findByText('Nothing in this category among the results loaded so far.'),
    ).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Load more results' }))
    await waitFor(() =>
      expect(api.GET).toHaveBeenCalledWith('/api/v1/search', {
        params: { query: { query: 'harbor', page: 2 } },
      }),
    )
  })

  it('does not end in a dead end without matches', async () => {
    serve([])
    renderAt('/search?q=zzz')
    expect(await screen.findByText('No matches')).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Back to Discover' }).getAttribute('href')).toBe('/')
  })

  it('offers the categories to browse before anything is typed', async () => {
    serve()
    renderAt('/search')
    expect(await screen.findByText('Find something to watch or listen to')).toBeTruthy()
    expect((await screen.findByRole('link', { name: 'Movies' })).getAttribute('href')).toBe(
      '/discover/movies',
    )
    expect(screen.getByRole('link', { name: 'Music' }).getAttribute('href')).toBe('/discover/music')
  })
})
