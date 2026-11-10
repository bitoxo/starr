import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { useLocation } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { MyRequest, TitleDetails } from '@/api/discover'
import type { Follow } from '@/api/follows'
import { DetailViewHost } from '@/components/DetailView'
import { expectFocusNotOnBody } from '@/test/focus'
import { NO_JOURNEY, ok, renderWithProviders } from '@/test/render'

import RequestsPage from './RequestsPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn(), DELETE: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

type ItemPatch = Omit<MyRequest['items'][number], 'journey'> &
  Partial<Pick<MyRequest['items'][number], 'journey'>>

function request(
  id: number,
  title: string,
  { items, ...patch }: Partial<Omit<MyRequest, 'items'>> & { items?: ItemPatch[] } = {},
): MyRequest {
  return {
    id,
    kind: 'movie',
    titleId: String(id),
    title,
    year: 2010,
    poster: null,
    category: 'movies',
    state: 'pending_approval',
    rejectReason: null,
    follow: false,
    canWithdraw: false,
    canCancel: false,
    canHide: false,
    hidden: false,
    createdAt: '2026-11-01T12:00:00Z',
    updatedAt: '2026-11-01T12:00:00Z',
    items: (items ?? [{ season: null, state: patch.state ?? 'pending_approval' }]).map((item) => ({
      journey: NO_JOURNEY,
      ...item,
    })),
    openIn: [],
    ...patch,
  }
}

const MINE = [
  request(1, 'Inception', { canWithdraw: true }),
  request(2, 'Dune', { state: 'in_progress', items: [{ state: 'in_progress', progress: 0.42 }] }),
  request(3, 'Cats', { state: 'rejected', rejectReason: 'Not on our server, sorry' }),
  request(4, 'Game of Thrones', {
    kind: 'series',
    category: 'series',
    state: 'partially_available',
    follow: true,
    items: [
      { season: 1, state: 'available' },
      { season: 2, state: 'in_progress', progress: 0.5 },
    ],
  }),
  request(5, 'Tenet', { state: 'failed' }),
]

const OPEN_IN: MyRequest['openIn'] = [
  { service: 'jellyfin', name: 'Kino', url: 'https://watch.example.com/v' },
]

const FOLLOWS: Follow[] = [
  {
    id: 7,
    kind: 'series',
    titleId: '1396',
    title: 'Breaking Bad',
    year: 2008,
    poster: null,
    category: 'series',
    seasonsThrough: 5,
    createdAt: '2026-10-01T12:00:00Z',
    checkedAt: null,
  },
]

/** Answer each GET by path, as the API would. */
function serve(requests: MyRequest[], follows: Follow[] = FOLLOWS) {
  api.GET.mockImplementation((path: string) =>
    Promise.resolve(ok(path === '/api/v1/follows' ? follows : requests)),
  )
}

function Search() {
  return <output data-testid="search">{useLocation().search}</output>
}

function search() {
  return screen.getByTestId('search').textContent
}

function titles() {
  return screen.getAllByRole('heading', { level: 3 }).map((h) => h.textContent)
}

/** The page's section headings (without "Following"). */
function sections() {
  return screen
    .getAllByRole('heading', { level: 2 })
    .map((h) => h.textContent)
    .filter((t) => t !== 'Following')
}

/** Open the collapsed "Closed" section. */
function openClosed() {
  fireEvent.click(screen.getByRole('button', { name: /^Closed/ }))
}

function row(title: string) {
  return screen.getByRole('heading', { name: new RegExp(title) }).closest('li') as HTMLElement
}

describe('Requests page', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    api.DELETE.mockReset()
    serve(MINE)
  })

  it('shows each state, progress, the rejection reason and follows', async () => {
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('heading', { name: /Inception/ })
    expect(within(row('Inception')).getByText('Waiting for approval')).toBeTruthy()
    expect(within(row('Inception')).getByText(/withdraw it until an admin approves/)).toBeTruthy()
    // On its way: the journey bar with the step and the share.
    const bar = within(row('Dune')).getByRole('img', { name: 'step 3 of 4, 42%' })
    expect(bar).toBeTruthy()
    expect(within(row('Dune')).getByText('42%')).toBeTruthy()
    // Failed reads in the member's words.
    expect(within(row('Tenet')).getByText('Failed')).toBeTruthy()
    expect(within(row('Tenet')).getByText(/Your admin has been told/)).toBeTruthy()
    // Partly there: a pill per season, the ready ones marked.
    const got = row('Game of Thrones')
    expect(within(got).getByText('Following new seasons')).toBeTruthy()
    // Ready to watch names what is there, not the season still on its way.
    expect(within(got).getByText('1 of 2 seasons')).toBeTruthy()
    const seasons = within(got).getByRole('list', { name: 'Seasons of Game of Thrones' })
    expect(within(seasons).getByText('S1')).toBeTruthy()
    expect(within(seasons).getByText('Season 1: Available')).toBeTruthy()
    expect(within(seasons).getByText('· 50%')).toBeTruthy()
    // Closed starts collapsed; opened it shows the reason.
    expect(screen.queryByRole('heading', { name: /Cats/ })).toBeNull()
    openClosed()
    expect(within(row('Cats')).getByText('Declined')).toBeTruthy()
    expect(within(row('Cats')).getByText('Reason: Not on our server, sorry')).toBeTruthy()
    // Only pending requests can be withdrawn; no gold in the rows.
    expect(screen.getAllByRole('button', { name: /Withdraw/ })).toHaveLength(1)
    for (const item of screen.getAllByRole('listitem')) {
      for (const control of within(item).queryAllByRole('button')) {
        expect(control.className).not.toContain('bg-polaris')
      }
    }
  })

  it('lays every row out alike: one thumb slot, chip over detail, one action column', async () => {
    serve([
      ...MINE,
      request(6, 'Arrival', {
        state: 'submitted',
        canCancel: true,
        items: [{ state: 'submitted' }],
      }),
      request(7, 'Verity', { state: 'available', canHide: true, openIn: OPEN_IN }),
    ])
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('heading', { name: /Inception/ })
    openClosed()
    const following = await screen.findByRole('region', { name: 'Following' })
    await within(following).findByRole('heading', { name: /Breaking Bad/ })
    const rows = screen.getAllByRole('heading', { level: 3 }).map((h) => h.closest('li'))
    expect(rows).toHaveLength(8)
    for (const item of rows as HTMLElement[]) {
      // The same 48 × 72 slot everywhere, whatever the section or the cover's shape.
      const slot = item.querySelector('[data-thumb]') as HTMLElement
      expect(slot.className).toContain('h-18')
      expect(slot.className).toContain('w-12')
      // One action column with the hide slot reserved, even without a hide action.
      const actions = item.querySelector('[data-actions]') as HTMLElement
      expect(actions.querySelector('[data-hide-slot]')).not.toBeNull()
      // Every action is quiet, with an icon and a label; never gold.
      const controls = [
        ...within(actions).queryAllByRole('button'),
        ...within(actions).queryAllByRole('link'),
      ]
      for (const control of controls) {
        expect(control.className).not.toContain('bg-polaris')
        expect(control.querySelector('svg')).not.toBeNull()
      }
    }
    // Without a hide action the slot stays, empty.
    const inceptionActions = row('Inception').querySelector('[data-actions]') as HTMLElement
    expect(inceptionActions.querySelector('[data-hide-slot]')?.childElementCount).toBe(0)
    // On its way: the state chip on top (it opens the journey, like the other sections'), the
    // journey bar below, named by its step and share.
    expect(within(row('Dune')).getByRole('button', { name: /Downloading/ })).toBeTruthy()
    expect(within(row('Dune')).getByRole('img', { name: 'step 3 of 4, 42%' })).toBeTruthy()
    expect(within(row('Arrival')).getByText('Searching')).toBeTruthy()
    // The actions keep their labels and sit in the action column.
    const actionsOf = (title: string) => row(title).querySelector('[data-actions]') as HTMLElement
    expect(
      within(actionsOf('Arrival')).getByRole('button', { name: /Cancel the request/ }),
    ).toBeTruthy()
    expect(within(actionsOf('Inception')).getByRole('button', { name: /Withdraw/ })).toBeTruthy()
    expect(within(actionsOf('Verity')).getByRole('link', { name: /Open in Kino/ })).toBeTruthy()
    // Hide is the rightmost slot: an icon button with a name.
    const hide = within(actionsOf('Verity')).getByRole('button', {
      name: 'Hide Verity from this list',
    })
    expect(hide.closest('[data-hide-slot]')).not.toBeNull()
    // Its tooltip says what it does, on keyboard focus too.
    act(() => hide.focus())
    expect(await screen.findByRole('tooltip', { name: 'Hide from this list' })).toBeTruthy()
    expect(
      within(following)
        .getByRole('button', { name: 'Stop following Breaking Bad' })
        .closest('[data-actions]'),
    ).not.toBeNull()
  })

  it('lists a music request by album, with a square cover', async () => {
    serve([
      request(9, 'Radiohead', {
        kind: 'artist',
        category: 'music',
        poster: 'b1392450-e666-3926-a536-22c65f834433',
        state: 'partially_available',
        items: [
          { album: { id: 'ok', title: 'OK Computer', year: 1997 }, state: 'available' },
          { album: { id: 'pool', title: 'A Moon Shaped Pool' }, state: 'submitted' },
        ],
      }),
      request(10, 'Björk', {
        kind: 'artist',
        category: 'music',
        state: 'pending_approval',
        items: [
          { album: { id: 'h', title: 'Homogenic' }, state: 'pending_approval' },
          { album: { id: 'v', title: 'Vespertine' }, state: 'pending_approval' },
        ],
      }),
    ])
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('heading', { name: /Radiohead/ })
    const albums = within(row('Radiohead')).getByRole('list', { name: 'Albums of Radiohead' })
    expect(within(albums).getByText('OK Computer')).toBeTruthy()
    expect(within(albums).getByText(/Available/)).toBeTruthy()
    expect(within(albums).getByText('A Moon Shaped Pool')).toBeTruthy()
    const cover = row('Radiohead').querySelector('img')
    expect(cover?.getAttribute('src')).toBe(
      '/api/v1/images/caa/500/b1392450-e666-3926-a536-22c65f834433',
    )
    expect(cover?.className).toContain('aspect-square')
    // All albums where the request is: one line with their titles.
    expect(within(row('Björk')).getByText('Homogenic, Vespertine')).toBeTruthy()
  })

  it('says a downloaded title is almost ready instead of downloading at 100%', async () => {
    serve([
      request(1, 'Verity', {
        state: 'in_progress',
        stage: 'arriving',
        arrivingIn: 'Jellyfin',
        items: [{ state: 'in_progress', progress: 1, stage: 'arriving' }],
      }),
      request(2, 'Dune', {
        state: 'in_progress',
        stage: 'importing',
        items: [{ state: 'in_progress', progress: 1, stage: 'importing' }],
      }),
      request(3, 'Game of Thrones', {
        kind: 'series',
        category: 'series',
        state: 'in_progress',
        arrivingIn: 'Jellyfin',
        items: [
          { season: 1, state: 'in_progress', progress: 1, stage: 'arriving' },
          { season: 2, state: 'in_progress', progress: 0.3 },
        ],
      }),
    ])
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('heading', { name: /Verity/ })
    expect(within(row('Verity')).getByText('Arriving in Jellyfin')).toBeTruthy()
    expect(within(row('Verity')).queryByRole('progressbar')).toBeNull()
    expect(within(row('Verity')).queryByText('Downloading')).toBeNull()
    expect(within(row('Dune')).getByText('Importing')).toBeTruthy()
    expect(within(row('Dune')).queryByRole('progressbar')).toBeNull()
    // Per season: one almost ready, one still loading with its bar.
    const seasons = within(row('Game of Thrones')).getByRole('list', {
      name: 'Seasons of Game of Thrones',
    })
    expect(within(seasons).getByText('Arriving in Jellyfin')).toBeTruthy()
    expect(within(seasons).getAllByRole('progressbar')).toHaveLength(1)
  })

  it('has one link per request with a readable name (MEM-27)', async () => {
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('heading', { name: /Inception/ })
    const links = within(row('Inception')).getAllByRole('link')
    expect(links).toHaveLength(1)
    expect(links[0]?.getAttribute('href')).toBe('/?title=movie-1')
    expect(screen.getByRole('link', { name: 'Inception (2010)' })).toBe(links[0])
    expect(screen.queryByRole('button', { name: /Details for/ })).toBeNull()
  })

  it('groups by phase with jump links, and filters the type in the URL (prototype 09-A)', async () => {
    renderWithProviders(
      <>
        <RequestsPage />
        <Search />
      </>,
      { path: '/requests' },
    )
    await screen.findByRole('heading', { name: /Inception/ })
    expect(sections()).toEqual([
      'On its way, 2',
      'Waiting for approval, 1',
      'Ready to watch, 1',
      'Closed, 1',
    ])
    // Failed counts as on its way (requestFilters.ts); closed rows wait behind their toggle.
    expect(titles()).toEqual([
      'Tenet (2010)',
      'Dune (2010)',
      'Inception (2010)',
      'Game of Thrones (2010)',
    ])
    const jump = screen.getByRole('navigation', { name: 'Jump to a group' })
    expect(
      within(jump)
        .getAllByRole('link')
        .map((l) => l.textContent),
    ).toEqual(['2 on its way', '1 waiting for approval', '1 ready to watch', '1 closed'])
    // Jumping to "Closed" opens it and moves focus to its toggle.
    fireEvent.click(within(jump).getByRole('link', { name: '1 closed' }))
    const toggle = screen.getByRole('button', { name: /^Closed/ })
    expect(toggle.getAttribute('aria-expanded')).toBe('true')
    expect(document.activeElement).toBe(toggle)
    expect(screen.getByRole('heading', { name: /Cats/ })).toBeTruthy()
    // Filters: only the type, in the URL.
    expect(screen.queryByRole('group', { name: 'Show requests' })).toBeNull()
    const categories = screen.getByRole('group', { name: 'Category' })
    fireEvent.click(within(categories).getByRole('button', { name: /^Series/ }))
    expect(search()).toBe('?category=series')
    expect(titles()).toEqual(['Game of Thrones (2010)'])
    fireEvent.click(within(categories).getByRole('button', { name: /^All/ }))
    expect(search()).toBe('')
  })

  it('keeps the type filter after a reload', async () => {
    renderWithProviders(<RequestsPage />, { path: '/requests?category=series' })
    await screen.findByRole('heading', { name: /Game of Thrones/ })
    expect(titles()).toEqual(['Game of Thrones (2010)'])
    const categories = screen.getByRole('group', { name: 'Category' })
    expect(
      within(categories)
        .getByRole('button', { name: /^Series/ })
        .getAttribute('aria-pressed'),
    ).toBe('true')
  })

  it('opens "Closed" for a closed request a notification points to; the toggle still works', async () => {
    renderWithProviders(<RequestsPage />, { path: '/requests?request=3' })
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('link', { name: 'Cats (2010)' })),
    )
    const toggle = screen.getByRole('button', { name: /^Closed/ })
    fireEvent.click(toggle)
    expect(toggle.getAttribute('aria-expanded')).toBe('false')
    expect(screen.queryByRole('heading', { name: /Cats/ })).toBeNull()
  })

  it('ignores an old ?state= link and shows everything', async () => {
    renderWithProviders(<RequestsPage />, { path: '/requests?state=closed' })
    await screen.findByRole('heading', { name: /Inception/ })
    expect(titles()).toHaveLength(4)
  })

  it('tells the member when and what in their words', async () => {
    const at = (step: string, status: 'done' | 'ended' | 'current' | 'warning', when?: string) => ({
      step,
      status,
      at: when ?? null,
    })
    const journey = (steps: ReturnType<typeof at>[]) =>
      ({ steps }) as MyRequest['items'][number]['journey']
    serve([
      request(1, 'Arrival', {
        state: 'submitted',
        canCancel: true,
        items: [{ state: 'submitted' }],
      }),
      request(2, 'Verity', {
        state: 'in_progress',
        items: [
          {
            state: 'in_progress',
            progress: 1,
            journey: journey([at('downloading', 'warning')]),
          },
        ],
      }),
      request(3, 'Tenet', {
        state: 'available',
        canHide: true,
        items: [
          {
            state: 'available',
            journey: journey([at('ready', 'done', '2026-10-29T10:00:00Z')]),
          },
        ],
      }),
      request(4, 'Cats', {
        state: 'cancelled',
        canHide: true,
        items: [
          {
            state: 'cancelled',
            journey: journey([at('withdrawn', 'ended', '2026-10-16T10:00:00Z')]),
          },
        ],
      }),
    ])
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('heading', { name: /Arrival/ })
    expect(within(row('Arrival')).getByRole('img', { name: 'step 2 of 4' })).toBeTruthy()
    expect(within(row('Verity')).getByText(/Taking longer than usual/)).toBeTruthy()
    expect(within(row('Tenet')).getByText(/^Ready since Oct 29/)).toBeTruthy()
    openClosed()
    expect(within(row('Cats')).getByText(/^You withdrew it on Oct 16/)).toBeTruthy()
  })

  it('highlights and focuses the request a notification points to (MEM-21)', async () => {
    renderWithProviders(<RequestsPage />, { path: '/requests?request=2' })
    await screen.findByRole('heading', { name: /Dune/ })
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('link', { name: 'Dune (2010)' })),
    )
    expect(row('Dune').getAttribute('data-highlighted')).toBe('true')
    expect(row('Inception').hasAttribute('data-highlighted')).toBe(false)
  })

  it('focuses the highlighted row once, not again after a filter change', async () => {
    renderWithProviders(<RequestsPage />, { path: '/requests?request=2' })
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('link', { name: 'Dune (2010)' })),
    )
    const categories = screen.getByRole('group', { name: 'Category' })
    const series = within(categories).getByRole('button', { name: /^Series/ })
    act(() => series.focus())
    fireEvent.click(series)
    const all = within(categories).getByRole('button', { name: /^All/ })
    act(() => all.focus())
    fireEvent.click(all)
    await screen.findByRole('link', { name: 'Dune (2010)' })
    expect(document.activeElement).toBe(all)
  })

  it('leaves focus to the detail overlay while it is open over the row', async () => {
    renderWithProviders(<RequestsPage />, { path: '/requests?request=2&title=movie-2' })
    await screen.findByRole('link', { name: 'Dune (2010)' })
    expect(row('Dune').getAttribute('data-highlighted')).toBe('true')
    expect(document.activeElement).not.toBe(screen.getByRole('link', { name: 'Dune (2010)' }))
  })

  it('opens the title’s details from the poster and the title', async () => {
    const details: TitleDetails = {
      kind: 'movie',
      id: '1',
      category: 'movies',
      title: 'Inception',
      originalTitle: 'Inception',
      year: 2010,
      overview: 'A thief who steals corporate secrets.',
      poster: null,
      backdrop: null,
      genres: [],
      rating: null,
      voteCount: 0,
      trailerUrl: null,
      status: 'available',
      requestedByMe: true,
      journeys: [],
      seasons: [],
      openIn: [{ service: 'jellyfin', name: 'Kino', url: 'https://watch.example.com/x' }],
      requestable: true,
      networks: [],
      cast: [],
      crew: [],
      watchProviders: [],
      details: { originalLanguage: 'en', companies: [], externalIds: {} },
      attributions: [],
    }
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(
        ok(
          path === '/api/v1/follows' ? [] : path === '/api/v1/titles/{kind}/{id}' ? details : MINE,
        ),
      ),
    )
    renderWithProviders(
      <>
        <RequestsPage />
        <DetailViewHost />
      </>,
    )
    fireEvent.click(await screen.findByRole('link', { name: 'Inception (2010)' }))
    const dialog = await screen.findByRole('dialog', { name: 'Inception' })
    expect(within(dialog).getByRole('link', { name: 'Open in Jellyfin' })).toBeTruthy()
  })

  it('withdraws a pending request', async () => {
    api.POST.mockResolvedValueOnce(ok({ ...MINE[0], state: 'cancelled', canWithdraw: false }))
    renderWithProviders(<RequestsPage />)
    fireEvent.click(
      await screen.findByRole('button', { name: 'Withdraw the pending part of Inception' }),
    )
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests/{id}/withdraw', {
        params: { path: { id: 1 } },
      }),
    )
  })

  it('cancels an approved request only after confirming', async () => {
    serve([request(8, 'Verity', { state: 'submitted', canCancel: true })])
    api.POST.mockResolvedValueOnce(ok({ state: 'cancelled' }))
    renderWithProviders(<RequestsPage />)
    fireEvent.click(await screen.findByRole('button', { name: 'Cancel the request for Verity' }))
    const dialog = await screen.findByRole('dialog', { name: 'Cancel Verity?' })
    expect(within(dialog).getByText(/Nothing is deleted/)).toBeTruthy()
    expect(api.POST).not.toHaveBeenCalled()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Keep it' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(api.POST).not.toHaveBeenCalled()

    fireEvent.click(screen.getByRole('button', { name: 'Cancel the request for Verity' }))
    const again = await screen.findByRole('dialog', { name: 'Cancel Verity?' })
    fireEvent.click(within(again).getByRole('button', { name: 'Cancel request' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests/{id}/cancel', {
        params: { path: { id: 8 } },
      }),
    )
  })

  it('hides finished requests and shows them again with "Show hidden"', async () => {
    serve([
      request(9, 'Cats', { state: 'revoked', canHide: true, revokeReason: 'Not on this server' }),
    ])
    api.PUT.mockResolvedValueOnce(ok({ hidden: true }))
    renderWithProviders(<RequestsPage />)
    fireEvent.click(await screen.findByRole('button', { name: /^Closed/ }))
    expect(await screen.findByText('Revoked by an admin. Reason: Not on this server')).toBeTruthy()
    expect(within(row('Cats')).getByText('Revoked')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Hide Cats from this list' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/requests/{id}/hidden', {
        params: { path: { id: 9 } },
        body: { hidden: true },
      }),
    )
    expect(api.GET).toHaveBeenCalledWith('/api/v1/requests', {
      params: { query: { hidden: false } },
    })
    serve([request(9, 'Cats', { state: 'revoked', canHide: true, hidden: true })])
    fireEvent.click(screen.getByRole('switch', { name: 'Show hidden' }))
    await waitFor(() =>
      expect(api.GET).toHaveBeenCalledWith('/api/v1/requests', {
        params: { query: { hidden: true } },
      }),
    )
    expect(await screen.findByText('Hidden')).toBeTruthy()
    // The row's own "Show again" (the section stays open while the list reloads).
    expect(screen.getByRole('button', { name: 'Show Cats in this list again' })).toBeTruthy()
  })

  it('moves focus on to the next section after hiding its last row', async () => {
    const tenet = request(3, 'Tenet', { state: 'available', canHide: true })
    const cats = request(9, 'Cats', { state: 'revoked', canHide: true })
    serve([tenet, cats])
    api.PUT.mockResolvedValueOnce(ok({ hidden: true }))
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('button', { name: /^Closed/ })
    openClosed()
    const hideTenet = screen.getByRole('button', { name: 'Hide Tenet from this list' })
    act(() => hideTenet.focus())
    serve([cats])
    fireEvent.click(hideTenet)
    await waitFor(() => expect(screen.queryByRole('heading', { name: /Tenet/ })).toBeNull())
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /^Closed/ }))
    expectFocusNotOnBody()
  })

  it('moves focus to the next request after hiding one (MEM-03)', async () => {
    const cats = request(9, 'Cats', { state: 'revoked', canHide: true })
    const dune = request(2, 'Dune', { state: 'cancelled', canHide: true })
    serve([cats, dune])
    api.PUT.mockResolvedValueOnce(ok({ hidden: true }))
    renderWithProviders(<RequestsPage />)
    fireEvent.click(await screen.findByRole('button', { name: /^Closed/ }))
    const hideCats = await screen.findByRole('button', { name: 'Hide Cats from this list' })
    act(() => hideCats.focus())
    serve([dune])
    fireEvent.click(hideCats)
    await waitFor(() => expect(screen.queryByRole('heading', { name: /Cats/ })).toBeNull())
    expect(row('Dune').contains(document.activeElement)).toBe(true)
    expectFocusNotOnBody()
  })

  it('points to Discover while there are none', async () => {
    serve([], [])
    renderWithProviders(<RequestsPage />)
    expect(await screen.findByText('No requests yet')).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Go to Discover' }).getAttribute('href')).toBe('/')
  })

  it('adds "Following" only once the requests are there, so nothing below jumps (slice 12)', async () => {
    let release: (value: unknown) => void = () => undefined
    api.GET.mockImplementation((path: string) =>
      path === '/api/v1/requests'
        ? new Promise((resolve) => {
            release = resolve
          })
        : Promise.resolve(ok(FOLLOWS)),
    )
    renderWithProviders(<RequestsPage />)
    await screen.findByRole('list', { name: /Loading/ })
    expect(screen.queryByRole('region', { name: 'Following' })).toBeNull()
    act(() => release(ok([])))
    expect(await screen.findByRole('region', { name: 'Following' })).toBeTruthy()
  })

  it('lists follows with unfollow', async () => {
    api.DELETE.mockResolvedValueOnce(ok(undefined, 204))
    renderWithProviders(<RequestsPage />)
    const following = await screen.findByRole('region', { name: 'Following' })
    expect(await within(following).findByRole('heading', { name: /Breaking Bad/ })).toBeTruthy()
    expect(within(following).getByText('New seasons after season 5')).toBeTruthy()
    fireEvent.click(within(following).getByRole('button', { name: 'Stop following Breaking Bad' }))
    await waitFor(() =>
      expect(api.DELETE).toHaveBeenCalledWith('/api/v1/follows/{id}', {
        params: { path: { id: 7 } },
      }),
    )
  })

  it('says when nothing is followed', async () => {
    serve(MINE, [])
    renderWithProviders(<RequestsPage />)
    expect(await screen.findByText(/don’t follow anything yet/)).toBeTruthy()
  })

  it('lists a followed artist with the day from which releases count', async () => {
    serve(MINE, [
      {
        id: 8,
        kind: 'artist',
        titleId: 'a74b1b7f-71a5-4011-9441-d0b5e4122711',
        title: 'Radiohead',
        poster: 'b1392450-e666-3926-a536-22c65f834433',
        category: 'music',
        knownThrough: '2026-11-03',
        createdAt: '2026-11-03T12:00:00Z',
      },
    ])
    renderWithProviders(<RequestsPage />)
    const following = await screen.findByRole('region', { name: 'Following' })
    expect(await within(following).findByRole('heading', { name: /Radiohead/ })).toBeTruthy()
    expect(within(following).getByText(/^New releases after .*2026$/)).toBeTruthy()
    // An artist's cover is square and round, through Starr's proxy.
    const cover = following.querySelector('img')
    expect(cover?.getAttribute('src')).toContain('/api/v1/images/caa/')
  })

  it('never shows or asks for private requests or follows, even unlocked (PRIV-16)', async () => {
    for (const unlocked of [false, true]) {
      api.GET.mockImplementation((path: string) =>
        Promise.resolve(
          ok(
            path === '/api/v1/auth/me'
              ? { kind: 'member', displayName: 'Ada', privateCategory: { unlocked } }
              : path === '/api/v1/follows'
                ? FOLLOWS
                : MINE,
          ),
        ),
      )
      const view = renderWithProviders(<RequestsPage />)
      await screen.findByRole('heading', { name: /Inception/ })
      expect(screen.queryByRole('button', { name: 'Private' })).toBeNull()
      expect(screen.queryByText(/Private/)).toBeNull()
      view.unmount()
    }
    const paths = api.GET.mock.calls.map((call) => String(call[0]))
    expect(paths.filter((p) => p.startsWith('/api/v1/private'))).toEqual([])
  })
})
