import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { Route, Routes, useLocation } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { DiscoverCategory, Highlight, TitleCard } from '@/api/discover'
import { fail, ok, renderWithProviders } from '@/test/render'

import CategoryPage from './CategoryPage'
import SeeAllPage from './SeeAllPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))
vi.mock('sonner', () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }))

const card = (id: number, extra: Partial<TitleCard> = {}): TitleCard => ({
  kind: 'movie',
  id: String(id),
  title: `Title ${id}`,
  year: 2026,
  poster: `p${id}.jpg`,
  backdrop: `b${id}.jpg`,
  status: 'new',
  requestedByMe: false,
  journeys: [],
  genres: [],
  ...extra,
})

const TRENDING = Array.from({ length: 20 }, (_, i) => card(i + 1))
const HERO: Highlight[] = TRENDING.slice(0, 10).map((c, i) => ({
  ...c,
  rank: i + 1,
  category: 'movies',
}))

const MOVIES: DiscoverCategory = {
  category: 'movies',
  kind: 'movie',
  discoverReady: true,
  requestable: true,
  lists: ['trending', 'popular', 'upcoming', 'top_rated'],
  providers: [
    { id: 8, name: 'Netflix', logo: 'nf.png' },
    { id: 337, name: 'Disney Plus', logo: null },
  ],
  genres: [
    { id: '18', name: 'Drama' },
    { id: '35', name: 'Comedy' },
  ],
  attributions: [
    { data: 'watch_providers', source: 'JustWatch', url: 'https://www.justwatch.com' },
  ],
}

const SERIES: DiscoverCategory = {
  ...MOVIES,
  category: 'series',
  kind: 'series',
}

const MUSIC: DiscoverCategory = {
  category: 'music',
  kind: 'album',
  discoverReady: true,
  requestable: true,
  lists: ['trending', 'fresh_releases', 'popular'],
  providers: [],
  genres: [
    { id: 'rock', name: 'Rock' },
    { id: 'jazz', name: 'Jazz' },
  ],
  attributions: [
    {
      data: 'genres',
      source: 'MusicBrainz',
      licence: 'CC BY-NC-SA 3.0',
      url: 'https://creativecommons.org/licenses/by-nc-sa/3.0/',
    },
  ],
}

type Opts = { params?: { path?: { list?: string }; query?: Record<string, unknown> } }

/** Serve the categories and per-list pages; `lists` maps a list name to its page or a failure. */
function serve(
  categories: DiscoverCategory[],
  lists: Record<string, TitleCard[] | 'fail'> = {},
  { hero = HERO, follows = [] as unknown[] } = {},
) {
  api.GET.mockImplementation((path: string, opts?: Opts) => {
    if (path === '/api/v1/discover') return Promise.resolve(ok(categories))
    if (path === '/api/v1/discover/highlights') return Promise.resolve(ok(hero))
    if (path === '/api/v1/follows') return Promise.resolve(ok(follows))
    if (path === '/api/v1/titles/{kind}/{id}') return new Promise(() => {})
    if (path === '/api/v1/discover/{category}/{list}') {
      const list = opts?.params?.path?.list ?? ''
      const found = lists[list] ?? [card(900 + list.length)]
      if (found === 'fail') return Promise.resolve(fail('catalog.unavailable', 503))
      return Promise.resolve(ok({ items: found, next: null }))
    }
    return Promise.resolve(ok([]))
  })
}

function Search() {
  return <output data-testid="search">{useLocation().search}</output>
}

function renderAt(path: string) {
  return renderWithProviders(
    <>
      <Routes>
        <Route path="/discover/:category" element={<CategoryPage />} />
        <Route path="/discover/:category/:list" element={<SeeAllPage />} />
      </Routes>
      <Search />
    </>,
    { path },
  )
}

const listCalls = (list: string) =>
  api.GET.mock.calls.filter(
    ([path, opts]) =>
      path === '/api/v1/discover/{category}/{list}' && (opts as Opts).params?.path?.list === list,
  )

describe('Category page (/discover/:category, spec 0008 slice 4)', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
  })

  it.each(['adult', 'podcasts'])('is "not found" for %s, like every unknown URL', async (c) => {
    serve([MOVIES])
    renderAt(`/discover/${c}`)
    expect(await screen.findByText('Nothing out here')).toBeTruthy()
  })

  it('is "not found" for a category the member may not browse', async () => {
    serve([MOVIES])
    renderAt('/discover/series')
    expect(await screen.findByText('Nothing out here')).toBeTruthy()
  })

  it('shows the category hero with its top 10 and "More trending" with ranks 11–20 (F-3)', async () => {
    serve([MOVIES], { trending: TRENDING })
    renderAt('/discover/movies')
    expect(await screen.findByRole('heading', { level: 1, name: 'Movies' })).toBeTruthy()
    await waitFor(() =>
      expect(api.GET).toHaveBeenCalledWith('/api/v1/discover/highlights', {
        params: { query: { category: 'movies' } },
      }),
    )
    const lane = await screen.findByRole('list', { name: 'Movies: More trending' })
    await waitFor(() => expect(within(lane).getAllByRole('button')).toHaveLength(10))
    const ranks = Array.from(lane.querySelectorAll('[data-rank]')).map((el) =>
      el.getAttribute('data-rank'),
    )
    expect(ranks).toEqual(['11', '12', '13', '14', '15', '16', '17', '18', '19', '20'])
    // The rank is in the cover's name; nothing of the hero's ten repeats.
    expect(within(lane).getByRole('button', { name: /^Number 11, Title 11 \(2026\)/ })).toBeTruthy()
    expect(within(lane).queryByRole('button', { name: /Title 1 \(/ })).toBeNull()
    // One tab stop for the whole lane (F4).
    expect(
      within(lane)
        .getAllByRole('button')
        .filter((b) => b.tabIndex === 0),
    ).toHaveLength(1)
  })

  it('ranks 1–10 under "Trending this week" when the hero has nothing', async () => {
    serve([MOVIES], { trending: TRENDING }, { hero: [] })
    renderAt('/discover/movies')
    const lane = await screen.findByRole('list', { name: 'Movies: Trending this week' })
    await waitFor(() => expect(lane.querySelector('[data-rank="1"]')).not.toBeNull())
    expect(lane.querySelector('[data-rank="10"]')).not.toBeNull()
    expect(lane.querySelector('[data-rank="11"]')).toBeNull()
    // Without a hero the page names itself visibly.
    expect(screen.getByRole('heading', { level: 1, name: 'Movies' }).className).not.toContain(
      'sr-only',
    )
  })

  it('switches "On <provider>" with the keyboard and keeps the provider in the URL (F-7, F6)', async () => {
    serve([MOVIES], {
      on_provider: [card(50, { title: 'Spot', overview: 'A spotlight title.' }), card(51)],
    })
    renderAt('/discover/movies')
    expect(await screen.findByRole('heading', { level: 2, name: 'On Netflix' })).toBeTruthy()
    await waitFor(() =>
      expect(api.GET).toHaveBeenCalledWith('/api/v1/discover/{category}/{list}', {
        params: {
          path: { category: 'movies', list: 'on_provider' },
          query: { page: 1, provider: 8 },
        },
      }),
    )
    const switcher = screen.getByRole('tablist', { name: 'Streaming service' })
    const tabs = within(switcher).getAllByRole('tab')
    expect(tabs.map((t) => t.tabIndex)).toEqual([0, -1])
    expect(tabs[0]?.getAttribute('aria-selected')).toBe('true')
    // The spotlight takes the first title; the lane the rest.
    expect(await screen.findByRole('heading', { level: 3, name: 'Spot' })).toBeTruthy()
    const lane = await screen.findByRole('list', { name: 'Movies: On Netflix' })
    expect(within(lane).queryByRole('button', { name: /^Spot/ })).toBeNull()

    act(() => tabs[0]?.focus())
    fireEvent.keyDown(tabs[0] as HTMLElement, { key: 'ArrowRight' })
    expect(await screen.findByRole('heading', { level: 2, name: 'On Disney Plus' })).toBeTruthy()
    expect(document.activeElement).toBe(within(switcher).getAllByRole('tab')[1])
    expect(screen.getByTestId('search').textContent).toBe('?provider=337')
    await waitFor(() =>
      expect(api.GET).toHaveBeenCalledWith('/api/v1/discover/{category}/{list}', {
        params: {
          path: { category: 'movies', list: 'on_provider' },
          query: { page: 1, provider: 337 },
        },
      }),
    )
    // "See all" carries the provider; the JustWatch credit is on the page.
    expect(
      screen.getByRole('link', { name: 'See all: Movies: On Disney Plus' }).getAttribute('href'),
    ).toBe('/discover/movies/on_provider?provider=337')
    expect(screen.getByRole('link', { name: 'Streaming data by JustWatch' })).toBeTruthy()
  })

  it('ignores a provider in the URL that is not in the switcher', async () => {
    serve([MOVIES])
    renderAt('/discover/movies?provider=999')
    expect(await screen.findByRole('heading', { level: 2, name: 'On Netflix' })).toBeTruthy()
  })

  it('links genre tiles to their grid', async () => {
    serve([MOVIES])
    renderAt('/discover/movies')
    const drama = await screen.findByRole('link', { name: 'Drama' })
    expect(drama.getAttribute('href')).toBe('/discover/movies/genre?genre=18')
    // The lift moves only with motion allowed (reduced motion: the ring alone).
    const moves = drama.className.split(' ').filter((c) => /translate|scale|transition/.test(c))
    expect(moves.length).toBeGreaterThan(0)
    expect(moves.every((c) => c.startsWith('motion-safe:'))).toBe(true)
    await waitFor(() => expect(listCalls('genre')).toHaveLength(2))
  })

  it('shows upcoming titles with their exact day', async () => {
    serve([MOVIES], { upcoming: [card(70, { releaseDate: '2026-11-13' })] })
    renderAt('/discover/movies')
    const lane = await screen.findByRole('list', { name: 'Movies: Upcoming' })
    expect(await within(lane).findByRole('button', { name: 'Title 70, Fri, Nov 13' })).toBeTruthy()
  })

  it('shows a failed lane with a retry, the rest of the page stays', async () => {
    serve([MOVIES], { top_rated: 'fail' })
    renderAt('/discover/movies')
    expect(await screen.findByText("This list couldn't load.")).toBeTruthy()
    expect(screen.getByRole('heading', { level: 2, name: 'Upcoming' })).toBeTruthy()
  })

  it('gives series "New episodes this week"', async () => {
    serve([SERIES])
    renderAt('/discover/series')
    expect(
      await screen.findByRole('heading', { level: 2, name: 'New episodes this week' }),
    ).toBeTruthy()
    await waitFor(() => expect(listCalls('on_the_air')).toHaveLength(1))
  })

  it('has a jump list to its sections', async () => {
    serve([MOVIES])
    renderAt('/discover/movies')
    const nav = await screen.findByRole('navigation', { name: 'On this page' })
    expect(
      within(nav)
        .getAllByRole('link')
        .map((a) => a.getAttribute('href')),
    ).toEqual(['#trending', '#provider', '#genres', '#top-rated', '#upcoming'])
    // "On <provider>" everywhere (F-7), also in the jump list.
    expect(within(nav).getByRole('link', { name: 'On Netflix' })).toBeTruthy()
  })

  it('leaves "More trending" out when the hero already shows every trending title', async () => {
    serve([MOVIES], { trending: TRENDING.slice(0, 10) })
    renderAt('/discover/movies')
    expect(await screen.findByRole('heading', { level: 2, name: 'On Netflix' })).toBeTruthy()
    await waitFor(() => expect(listCalls('trending').length).toBeGreaterThan(0))
    await waitFor(() =>
      expect(screen.queryByRole('heading', { level: 2, name: 'More trending' })).toBeNull(),
    )
  })

  it('ranks 1–10 and offers a retry when the hero fails', async () => {
    serve([MOVIES], { trending: TRENDING })
    const base = api.GET.getMockImplementation() as (p: string, o?: Opts) => Promise<unknown>
    api.GET.mockImplementation((path: string, opts?: Opts) =>
      path === '/api/v1/discover/highlights'
        ? Promise.resolve(fail('catalog.unavailable', 503))
        : base(path, opts),
    )
    renderAt('/discover/movies')
    expect(await screen.findByText("The top 10 couldn't load.")).toBeTruthy()
    const lane = await screen.findByRole('list', { name: 'Movies: Trending this week' })
    await waitFor(() => expect(lane.querySelector('[data-rank="1"]')).not.toBeNull())
  })

  it('moves focus to the first title after a lane\'s "Try again" (F3)', async () => {
    let failing = true
    serve([MOVIES])
    const base = api.GET.getMockImplementation() as (p: string, o?: Opts) => Promise<unknown>
    api.GET.mockImplementation((path: string, opts?: Opts) => {
      if (opts?.params?.path?.list === 'top_rated') {
        return Promise.resolve(
          failing ? fail('catalog.unavailable', 503) : ok({ items: [card(77)], next: null }),
        )
      }
      return base(path, opts)
    })
    renderAt('/discover/movies')
    const section = (await screen.findByRole('heading', { level: 2, name: 'Top rated' }))
      .parentElement?.parentElement as HTMLElement
    const retry = await within(section).findByRole('button', { name: 'Try again' })
    failing = false
    act(() => retry.focus())
    fireEvent.click(retry)
    await waitFor(() =>
      expect(document.activeElement?.getAttribute('aria-label')).toMatch(/^Title 77/),
    )
  })

  it('moves through the providers with Home and End', async () => {
    serve([MOVIES])
    renderAt('/discover/movies')
    const switcher = await screen.findByRole('tablist', { name: 'Streaming service' })
    const tabs = within(switcher).getAllByRole('tab')
    act(() => tabs[0]?.focus())
    fireEvent.keyDown(tabs[0] as HTMLElement, { key: 'End' })
    expect(document.activeElement).toBe(tabs[1])
    expect(screen.getByTestId('search').textContent).toBe('?provider=337')
    fireEvent.keyDown(tabs[1] as HTMLElement, { key: 'Home' })
    expect(document.activeElement).toBe(tabs[0])
    // The first provider is the default: not written to the URL.
    expect(screen.getByTestId('search').textContent).toBe('')
  })

  it('shows eight genre tiles, the rest behind "Show all genres"', async () => {
    const genres = Array.from({ length: 10 }, (_, i) => ({ id: String(i + 1), name: `G${i + 1}` }))
    serve([{ ...MOVIES, genres }])
    renderAt('/discover/movies')
    const toggle = await screen.findByRole('button', { name: 'Show all genres' })
    const tiles = () => document.querySelectorAll('[data-genre-tile]')
    expect(tiles()).toHaveLength(8)
    expect(toggle.getAttribute('aria-expanded')).toBe('false')
    fireEvent.click(toggle)
    expect(tiles()).toHaveLength(10)
    expect(toggle.getAttribute('aria-expanded')).toBe('true')
    expect(toggle.textContent).toBe('Show fewer genres')
    // Until a cover's colour is read, the tile is the plain surface (no tint, never a category colour).
    expect((tiles()[0] as HTMLElement).getAttribute('style')).toBeNull()
  })

  describe('music', () => {
    const albums = Array.from({ length: 12 }, (_, i) =>
      card(i + 1, { kind: 'album', poster: `mbid-${i}`, backdrop: null, artist: 'A' }),
    )

    it('has no hero, ranks 1–10, genre lanes and the MusicBrainz credit (Q17 B)', async () => {
      serve([MUSIC], { trending: albums })
      renderAt('/discover/music')
      expect(await screen.findByRole('heading', { level: 1, name: 'Music' })).toBeTruthy()
      const lane = await screen.findByRole('list', { name: 'Music: Trending this week' })
      await waitFor(() => expect(lane.querySelector('[data-rank="1"]')).not.toBeNull())
      expect(api.GET).not.toHaveBeenCalledWith('/api/v1/discover/highlights', expect.anything())
      // A lane per genre, switched by a tab list.
      const genres = screen.getByRole('tablist', { name: 'Genre' })
      expect(
        within(genres)
          .getAllByRole('tab')
          .map((t) => t.textContent),
      ).toEqual(['Rock', 'Jazz'])
      await waitFor(() =>
        expect(api.GET).toHaveBeenCalledWith('/api/v1/discover/{category}/{list}', {
          params: { path: { category: 'music', list: 'genre' }, query: { page: 1, genre: 'rock' } },
        }),
      )
      const credit = screen.getByRole('link', {
        name: 'Genres from MusicBrainz (CC BY-NC-SA)',
      })
      expect(credit.getAttribute('href')).toBe('https://creativecommons.org/licenses/by-nc-sa/3.0/')
    })

    it('explains "From artists you follow" without follows', async () => {
      serve([MUSIC])
      renderAt('/discover/music')
      expect(
        await screen.findByText('Follow an artist from their page, and their albums show up here.'),
      ).toBeTruthy()
    })

    it('switches between followed artists and shows their albums', async () => {
      const follows = [
        {
          id: 1,
          kind: 'artist',
          category: 'music',
          title: 'Juno Lark',
          titleId: 'ar-1',
          createdAt: '',
        },
        {
          id: 2,
          kind: 'artist',
          category: 'music',
          title: 'Kasimir',
          titleId: 'ar-2',
          createdAt: '',
        },
      ]
      serve([MUSIC], {}, { follows })
      api.GET.mockImplementation(
        ((base) => (path: string, opts?: { params?: { path?: { id?: string } } }) => {
          if (path === '/api/v1/artists/{id}') {
            const id = opts?.params?.path?.id
            return Promise.resolve(
              ok({
                id,
                name: id === 'ar-1' ? 'Juno Lark' : 'Kasimir',
                albums: [
                  {
                    id: `${id}-al`,
                    title: `${id} album`,
                    year: 2026,
                    inDefaultView: true,
                    requestedByMe: false,
                    secondaryTypes: [],
                    status: 'new',
                  },
                ],
                journeys: [],
                genres: [],
                attributions: [],
                category: 'music',
                disambiguation: '',
                requestable: true,
              }),
            )
          }
          return base(path, opts)
        })(api.GET.getMockImplementation() as (p: string, o?: unknown) => Promise<unknown>),
      )
      renderAt('/discover/music')
      // The lane remounts per artist, so it is looked up again each time.
      expect(
        await screen.findByRole('button', { name: /^ar-1 album \(2026\), Juno Lark/ }),
      ).toBeTruthy()
      const tabs = within(screen.getByRole('tablist', { name: 'Artist' })).getAllByRole('tab')
      fireEvent.click(tabs[1] as HTMLElement)
      expect(screen.getByTestId('search').textContent).toBe('?artist=ar-2')
      const lane = screen.getByRole('list', { name: 'Music: From artists you follow' })
      expect(await within(lane).findByRole('button', { name: /^ar-2 album/ })).toBeTruthy()
    })

    it('puts future releases into "Out soon" with their day', async () => {
      serve([MUSIC], {
        fresh_releases: [
          card(1, { kind: 'album', title: 'Later', releaseDate: '2099-01-02', backdrop: null }),
          card(2, { kind: 'album', title: 'Past', releaseDate: '2020-01-01', backdrop: null }),
        ],
      })
      renderAt('/discover/music')
      const soon = await screen.findByRole('list', { name: 'Music: Out soon' })
      expect(within(soon).getByRole('button', { name: /^Later, .*Jan 2/ })).toBeTruthy()
      const fresh = screen.getByRole('list', { name: 'Music: Fresh releases' })
      expect(within(fresh).getByRole('button', { name: /^Past/ })).toBeTruthy()
      expect(within(fresh).queryByRole('button', { name: /^Later/ })).toBeNull()
    })
  })
})

describe('See all (/discover/:category/:list, spec 0008 Q6 B)', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
  })

  const lastQuery = (list: string) =>
    (listCalls(list).at(-1)?.[1] as Opts | undefined)?.params?.query

  it.each(['nonsense', 'fresh_releases', 'on_the_air'])(
    'is "not found" for the list %s of movies',
    async (list) => {
      serve([MOVIES])
      renderAt(`/discover/movies/${list}`)
      expect(await screen.findByText('Nothing out here')).toBeTruthy()
    },
  )

  it('shows a captioned grid as one tab stop, with filters in the URL', async () => {
    serve([MOVIES], { popular: [card(1), card(2), card(3)] })
    renderAt('/discover/movies/popular')
    expect(await screen.findByRole('heading', { level: 1, name: 'Popular' })).toBeTruthy()
    const grid = await screen.findByRole('list', { name: 'Movies: Popular' })
    // Grids keep their caption (A3).
    expect(within(grid).getByText('Title 1')).toBeTruthy()
    expect(
      within(grid)
        .getAllByRole('button')
        .filter((b) => b.tabIndex === 0),
    ).toHaveLength(1)
    expect(lastQuery('popular')).toEqual({ page: 1 })

    fireEvent.click(screen.getByRole('radio', { name: 'Comedy' }))
    expect(screen.getByTestId('search').textContent).toBe('?genre=35')
    await waitFor(() => expect(lastQuery('popular')).toEqual({ page: 1, genre: '35' }))

    fireEvent.click(screen.getByRole('radio', { name: 'Netflix' }))
    fireEvent.change(screen.getByRole('combobox', { name: 'Sort by' }), {
      target: { value: 'rating' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Hide what I have' }))
    expect(
      screen.getByRole('button', { name: 'Hide what I have' }).getAttribute('aria-pressed'),
    ).toBe('true')
    await waitFor(() =>
      expect(lastQuery('popular')).toEqual({
        page: 1,
        provider: 8,
        genre: '35',
        sort: 'rating',
        hideOwned: true,
      }),
    )
    const search: Record<string, string> = {}
    new URLSearchParams(screen.getByTestId('search').textContent ?? '').forEach((v, k) => {
      search[k] = v
    })
    expect(search).toEqual({
      genre: '35',
      provider: '8',
      sort: 'rating',
      hideOwned: 'true',
    })

    // Clearing keeps focus in the toolbar and drops every filter.
    fireEvent.click(screen.getByRole('button', { name: 'Clear filters' }))
    expect(screen.getByTestId('search').textContent).toBe('')
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Hide what I have' }))
  })

  it('restores the filters from a shared link and ignores invalid ones', async () => {
    serve([MOVIES], { top_rated: [card(1)] })
    renderAt('/discover/movies/top_rated?genre=18&sort=bogus&provider=12345')
    expect(await screen.findByRole('heading', { level: 1, name: 'Top rated' })).toBeTruthy()
    await waitFor(() => expect(lastQuery('top_rated')).toEqual({ page: 1, genre: '18' }))
    expect(screen.getByRole('radio', { name: 'Drama' }).getAttribute('aria-checked')).toBe('true')
    expect((screen.getByRole('combobox', { name: 'Sort by' }) as HTMLSelectElement).value).toBe(
      'rating',
    )
  })

  it('"On <provider>" always has a provider, without "All services"', async () => {
    serve([MOVIES], { on_provider: [card(1)] })
    renderAt('/discover/movies/on_provider?provider=337')
    expect(await screen.findByRole('heading', { level: 1, name: 'On Disney Plus' })).toBeTruthy()
    expect(screen.queryByRole('radio', { name: 'All services' })).toBeNull()
    await waitFor(() => expect(lastQuery('on_provider')).toEqual({ page: 1, provider: 337 }))
  })

  it('offers only "Hide what I have" where the list takes no filter', async () => {
    serve([MOVIES], { trending: [card(1)] })
    renderAt('/discover/movies/trending')
    expect(await screen.findByRole('heading', { level: 1, name: 'Trending' })).toBeTruthy()
    expect(screen.queryByRole('radiogroup')).toBeNull()
    expect(screen.queryByRole('combobox', { name: 'Sort by' })).toBeNull()
    expect(screen.getByRole('button', { name: 'Hide what I have' })).toBeTruthy()
  })

  it('switches the music genre of a genre grid', async () => {
    serve([MUSIC], { genre: [card(1, { kind: 'album', backdrop: null })] })
    renderAt('/discover/music/genre?genre=jazz')
    expect(await screen.findByRole('heading', { level: 1, name: 'Jazz' })).toBeTruthy()
    await waitFor(() => expect(lastQuery('genre')).toEqual({ page: 1, genre: 'jazz' }))
    expect(screen.queryByRole('radio', { name: 'All genres' })).toBeNull()
    expect(screen.queryByRole('combobox', { name: 'Sort by' })).toBeNull()
  })

  it('explains an empty result and clears the filters from there', async () => {
    serve([MOVIES], { popular: [] })
    renderAt('/discover/movies/popular?hideOwned=true')
    expect(await screen.findByText('Nothing to show')).toBeTruthy()
    const clear = screen.getAllByRole('button', { name: 'Clear filters' })
    fireEvent.click(clear[clear.length - 1] as HTMLElement)
    expect(screen.getByTestId('search').textContent).toBe('')
  })

  it('shows a failed grid with a retry', async () => {
    serve([MOVIES], { popular: 'fail' })
    renderAt('/discover/movies/popular')
    expect(await screen.findByRole('alert')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy()
  })

  it('loads more by hand and moves focus to the first new title', async () => {
    const first = Array.from({ length: 3 }, (_, i) => card(i + 1))
    const second = [card(10), card(11)]
    serve([MOVIES])
    const base = api.GET.getMockImplementation() as (p: string, o?: Opts) => Promise<unknown>
    api.GET.mockImplementation((path: string, opts?: Opts) => {
      if (path === '/api/v1/discover/{category}/{list}') {
        const page = opts?.params?.query?.page
        return Promise.resolve(
          ok(page === 1 ? { items: first, next: 2 } : { items: second, next: null }),
        )
      }
      return base(path, opts)
    })
    renderAt('/discover/movies/popular')
    const more = await screen.findByRole('button', { name: 'Load more' })
    act(() => more.focus())
    fireEvent.click(more)
    await waitFor(() =>
      expect(document.activeElement?.getAttribute('aria-label')).toMatch(/^Title 10/),
    )
    expect(screen.queryByRole('button', { name: 'Load more' })).toBeNull()
  })

  it('puts focus on "Try again" when a page loaded by hand fails (F3)', async () => {
    serve([MOVIES])
    const base = api.GET.getMockImplementation() as (p: string, o?: Opts) => Promise<unknown>
    api.GET.mockImplementation((path: string, opts?: Opts) => {
      if (path === '/api/v1/discover/{category}/{list}') {
        return Promise.resolve(
          opts?.params?.query?.page === 1
            ? ok({ items: [card(1)], next: 2 })
            : fail('catalog.unavailable', 503),
        )
      }
      return base(path, opts)
    })
    renderAt('/discover/movies/popular')
    const more = await screen.findByRole('button', { name: 'Load more' })
    act(() => more.focus())
    fireEvent.click(more)
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Try again' })),
    )
  })

  it('names its way back', async () => {
    serve([MOVIES], { popular: [card(1)] })
    renderAt('/discover/movies/popular')
    const back = await screen.findByRole('link', { name: 'Back to Movies' })
    expect(back.getAttribute('href')).toBe('/discover/movies')
  })
})
