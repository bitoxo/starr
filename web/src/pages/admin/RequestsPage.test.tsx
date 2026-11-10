import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { Route, Routes, useLocation } from 'react-router'

import type { Acquisition, AdminRequest, AdminRequestItem } from '@/api/requests'
import { PosterThumb } from '@/components/PosterThumb'
import { NO_JOURNEY, ok, renderWithProviders } from '@/test/render'

import RequestsPage from './RequestsPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))
const toast = vi.hoisted(() =>
  Object.assign(vi.fn(), { error: vi.fn(), dismiss: vi.fn(), custom: vi.fn() }),
)
vi.mock('sonner', () => ({ toast }))

function item(id: number, patch: Partial<AdminRequestItem> = {}): AdminRequestItem {
  return {
    id,
    season: null,
    decision: 'pending_approval',
    state: 'pending_approval',
    journey: NO_JOURNEY,
    ...patch,
  }
}

function request(id: number, title: string, patch: Partial<AdminRequest> = {}): AdminRequest {
  return {
    id,
    member: { id: 7, username: 'anna', displayName: 'Anna Keller' },
    redacted: false,
    kind: 'movie',
    titleId: String(id),
    title,
    year: 2010,
    poster: null,
    category: 'movies',
    state: 'pending_approval',
    follow: false,
    hidden: false,
    canRevoke: true,
    createdAt: '2026-11-01T12:00:00Z',
    updatedAt: '2026-11-01T12:00:00Z',
    items: [item(id * 10)],
    ...patch,
  }
}

const WAITING = request(1, 'Inception')
const APPROVED = request(2, 'Arrival', {
  state: 'approved',
  member: { id: 8, username: 'ben', displayName: 'Ben Okafor' },
  items: [item(20, { decision: 'approved', state: 'approved' })],
})
const FAILED = request(3, 'Dune', {
  state: 'failed',
  canRevoke: true,
  items: [
    item(30, { decision: 'approved', state: 'failed', acquisitionId: 44, lastError: 'HTTP 400' }),
  ],
})
const HIDDEN = request(9, 'Private category · request #9', {
  redacted: true,
  category: 'adult',
  member: { id: 3, username: '', displayName: 'A member' },
})

function serve(list: AdminRequest[], me: unknown = null) {
  api.GET.mockImplementation((path: string, init?: { params?: { path?: { id?: number } } }) => {
    if (path === '/api/v1/admin/requests') return Promise.resolve(ok(list))
    if (path === '/api/v1/admin/requests/{id}') {
      const found = list.find((r) => r.id === init?.params?.path?.id)
      return Promise.resolve(ok({ request: found, events: [] }))
    }
    if (path === '/api/v1/auth/me') return Promise.resolve(ok(me))
    return Promise.resolve(ok([]))
  })
}

function Where() {
  const location = useLocation()
  return <output data-testid="where">{location.pathname + location.search}</output>
}

function page(path = '/admin/requests', me?: unknown) {
  return renderWithProviders(
    <Routes>
      <Route
        path="*"
        element={
          <>
            <RequestsPage />
            <Where />
          </>
        }
      />
    </Routes>,
    { path, me },
  )
}

const where = () => screen.getByTestId('where').textContent

describe('admin Requests on the DataTable (ADM-10)', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    toast.mockReset()
  })

  it('searches, filters and sorts with everything in the URL and a count', async () => {
    serve([WAITING, APPROVED, FAILED])
    page('/admin/requests?sort=-title')
    await screen.findByText('3 entries')
    const table = screen.getByRole('table', { name: 'Requests' })
    const titles = () =>
      within(table)
        .getAllByRole('button', { name: /^Open / })
        .map((b) => b.textContent?.replace(/\d{4}$/, ''))
    expect(titles()).toEqual(['Inception', 'Dune', 'Arrival'])
    expect(screen.getByText('3 entries')).toBeTruthy()
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search title or member' }), {
      target: { value: 'ben' },
    })
    expect(screen.getByText('1 of 3 entries')).toBeTruthy()
    expect(where()).toContain('q=ben')
    fireEvent.click(screen.getByRole('radio', { name: 'Series' }))
    await waitFor(() => expect(where()).toContain('category=series'))
    expect(api.GET).toHaveBeenLastCalledWith('/api/v1/admin/requests', {
      params: { query: { state: undefined, category: 'series', memberId: undefined } },
    })
  })

  it('calls the private category "Private" and links the member (one link per row)', async () => {
    serve([WAITING])
    page()
    await screen.findByText('1 entry')
    expect(screen.getByRole('radio', { name: 'Private' })).toBeTruthy()
    expect(screen.queryByRole('radio', { name: 'NSFW' })).toBeNull()
    expect(screen.getByRole('link', { name: 'Open Anna Keller' }).getAttribute('href')).toBe(
      '/admin/people/members/7',
    )
    // The state chip is plain: no popover button per row.
    expect(screen.queryByRole('button', { name: /Waiting for approval/ })).toBeNull()
  })

  it('keeps a search out of the URL while the private area is open (F6)', async () => {
    serve([WAITING], {
      kind: 'admin',
      username: 'root',
      displayName: 'Root',
      privateCategory: { unlocked: true, unlockedUntil: '2026-11-07T21:14:00Z' },
    })
    page('/admin/requests', {
      kind: 'admin',
      username: 'root',
      displayName: 'Root',
      privateCategory: { unlocked: true, unlockedUntil: '2026-11-07T21:14:00Z' },
    })
    await screen.findByRole('table', { name: 'Requests' })
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search title or member' }), {
      target: { value: 'scene' },
    })
    expect(where()).not.toContain('q=')
  })

  it('offers bulk actions per what is chosen, never Revoke for waiting only (ADM-31)', async () => {
    serve([WAITING, APPROVED, HIDDEN])
    page()
    await screen.findByText('3 entries')
    // A neutral private row is never chosen blind.
    expect(screen.queryByRole('checkbox', { name: /Private category/ })).toBeNull()
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select Inception' }))
    let bar = screen.getByRole('region', { name: '1 selected' })
    expect(within(bar).getByRole('button', { name: 'Approve 1' })).toBeTruthy()
    expect(within(bar).getByRole('button', { name: 'Decline 1' })).toBeTruthy()
    expect(within(bar).getByRole('button', { name: 'Revoke 1' })).toBeTruthy()
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select Arrival' }))
    bar = screen.getByRole('region', { name: '2 selected' })
    expect(within(bar).getByRole('button', { name: 'Approve 1' })).toBeTruthy()
    expect(within(bar).getByRole('button', { name: 'Revoke 2' })).toBeTruthy()
    api.POST.mockResolvedValue(ok({ decided: [1], skipped: [], undoable: false, held: [] }))
    fireEvent.click(within(bar).getByRole('button', { name: 'Approve 1' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/approvals/approve', {
        body: { requests: [{ id: 1, memberId: 7 }] },
      }),
    )
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith('Approved: Inception', { id: undefined }),
    )
  })

  it('opens the detail with the action bar of its state (ADM-04)', async () => {
    serve([WAITING, FAILED])
    page('/admin/requests?open=3')
    const sheet = await screen.findByRole('dialog')
    await within(sheet).findByRole('button', { name: 'Submit again' })
    expect(within(sheet).queryByRole('button', { name: 'Approve' })).toBeNull()
    expect(within(sheet).getByRole('link', { name: 'Open download' }).getAttribute('href')).toBe(
      '/admin/downloads?open=44',
    )
    api.POST.mockResolvedValue(ok({ id: 44 }))
    fireEvent.click(within(sheet).getByRole('button', { name: 'Submit again' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/acquisitions/{id}/retry', {
        params: { path: { id: 44 } },
      }),
    )
  })

  it('lets a waiting request be approved or declined from its detail', async () => {
    serve([WAITING])
    page('/admin/requests?open=1')
    const sheet = await screen.findByRole('dialog')
    const approve = await within(sheet).findByRole('button', { name: 'Approve' })
    expect(approve.className).toContain('bg-polaris')
    expect(sheet.querySelectorAll('.bg-polaris')).toHaveLength(1)
    expect(within(sheet).getByRole('button', { name: 'Decline' })).toBeTruthy()
  })

  it('gives every row the same thumb slot, music and private rows too', async () => {
    const MUSIC = request(4, 'Radiohead', {
      kind: 'artist',
      category: 'music',
      poster: 'mbid-1',
      state: 'approved',
      items: [item(40, { decision: 'approved', state: 'approved' })],
    })
    serve([WAITING, MUSIC, HIDDEN])
    page()
    await screen.findByText('3 entries')
    const table = screen.getByRole('table', { name: 'Requests' })
    const rows = within(table).getAllByRole('row').slice(1)
    expect(rows).toHaveLength(3)
    for (const row of rows) {
      // One 32 × 48 slot in every row, whatever the cover's shape (or the lock).
      const slot = row.querySelector('[data-thumb]') as HTMLElement
      expect(slot.className).toContain('h-12')
      expect(slot.className).toContain('w-8')
      expect(slot.childElementCount).toBe(1)
    }
    // The square music cover fills the slot's width and sits centred in it.
    const cover = rows[1]?.querySelector('[data-thumb] img') as HTMLElement
    expect(cover.className).toContain('aspect-square')
    expect(cover.className).toContain('w-full')
  })

  it('shows the state as a chip over one detail line', async () => {
    const steps = (step: string, status: string, extra: object = {}) => ({
      steps: [{ step, status, ...extra }],
    })
    const DOWNLOADING = request(5, 'Tenet', {
      state: 'in_progress',
      items: [
        item(50, {
          decision: 'approved',
          state: 'in_progress',
          progress: 0.42,
          journey: steps('downloading', 'current', { progress: 0.42 }) as never,
        }),
      ],
    })
    const HELD = request(6, 'OK Computer', {
      state: 'approved',
      category: 'music',
      kind: 'artist',
      items: [
        item(60, {
          decision: 'approved',
          state: 'approved',
          journey: { source: 'Lidarr', steps: [{ step: 'held', status: 'current' }] } as never,
        }),
      ],
    })
    const reason = 'Not on our servers, the family plan does not cover four-hour director cuts.'
    const DECLINED = request(7, 'Heat', {
      state: 'rejected',
      canRevoke: false,
      rejectReason: reason,
    })
    serve([WAITING, DOWNLOADING, HELD, DECLINED, FAILED])
    page()
    await screen.findByText('5 entries')
    const stateOf = (title: string) => {
      const row = screen
        .getByRole('button', { name: `Open ${title}` })
        .closest('[role="row"]') as HTMLElement
      return row.querySelector('[data-state-cell]') as HTMLElement
    }
    // Downloading: the chip names the state, the bar below carries the share.
    expect(within(stateOf('Tenet')).getByText('Downloading')).toBeTruthy()
    expect(within(stateOf('Tenet')).getByRole('progressbar').getAttribute('aria-valuenow')).toBe(
      '42',
    )
    // Held: the chip from the state table, the line says what happens next.
    expect(within(stateOf('OK Computer')).getByText('Held – Lidarr paused')).toBeTruthy()
    expect(within(stateOf('OK Computer')).getByText('Goes out once writes resume')).toBeTruthy()
    // Declined: the reason on one truncated line, in full as a tooltip (and in the sheet).
    const line = within(stateOf('Heat')).getByText(reason)
    expect(line.className).toContain('truncate')
    expect(line.getAttribute('title')).toBe(reason)
    // Failed: the source's error as the admin's hint.
    expect(within(stateOf('Dune')).getByText('HTTP 400')).toBeTruthy()
    // Nothing more to say: the chip alone.
    expect(stateOf('Inception').childElementCount).toBe(1)
  })

  it('names the reason, the source message or a hint for the other detail lines', async () => {
    const REVOKED = request(10, 'Heat', {
      state: 'revoked',
      canRevoke: false,
      revokeReason: 'Approved by mistake',
    })
    const STUCK = request(11, 'Ronin', {
      state: 'in_progress',
      items: [
        item(110, {
          decision: 'approved',
          state: 'in_progress',
          sourceMessage: 'No files found are eligible for import',
          journey: { steps: [{ step: 'importing', status: 'warning' }] } as never,
        }),
      ],
    })
    const IMPORTING = request(12, 'Collateral', {
      state: 'in_progress',
      stage: 'importing',
      items: [item(120, { decision: 'approved', state: 'in_progress', progress: 1 })],
    })
    // A neutral private row: the server leaves the error out; it never reached the source.
    const PRIVATE_FAILED = request(13, 'Private category · request #13', {
      redacted: true,
      category: 'adult',
      state: 'failed',
      canRevoke: false,
      member: { id: 3, username: '', displayName: 'A member' },
      items: [item(130, { decision: 'approved', state: 'failed' })],
    })
    serve([REVOKED, STUCK, IMPORTING, PRIVATE_FAILED])
    page()
    await screen.findByText('4 entries')
    const stateOf = (title: string) => {
      const row = screen
        .getByRole('button', { name: `Open ${title}` })
        .closest('[role="row"]') as HTMLElement
      return row.querySelector('[data-state-cell]') as HTMLElement
    }
    expect(within(stateOf('Heat')).getByText('Approved by mistake')).toBeTruthy()
    expect(within(stateOf('Ronin')).getByText('Stuck at import')).toBeTruthy()
    expect(
      within(stateOf('Ronin')).getByText('No files found are eligible for import'),
    ).toBeTruthy()
    // Importing: the chip alone, no bar at 100 %.
    expect(within(stateOf('Collateral')).getByText('Importing')).toBeTruthy()
    expect(within(stateOf('Collateral')).queryByRole('progressbar')).toBeNull()
    // No error and nothing to submit again: a plain hint, no promise of a button.
    const hidden = stateOf('Private category · request #13')
    expect(within(hidden).getByText('Failed at the source.')).toBeTruthy()
    expect(within(hidden).queryByText(/Submit again/)).toBeNull()
  })

  it('stacks rows as cards on phones (ADM-09)', async () => {
    serve([WAITING])
    page()
    await screen.findByText('1 entry')
    const table = screen.getByRole('table', { name: 'Requests' })
    const row = within(table).getAllByRole('row')[1] as HTMLElement
    expect(row.className).toContain('max-md:flex-col')
  })
})

describe('Downloads (held, failed, stuck filters)', () => {
  const acquisition = (id: number, patch: Partial<Acquisition>): Acquisition =>
    ({
      id,
      redacted: false,
      title: `Title ${id}`,
      category: 'movies',
      state: 'downloading',
      submitAttempts: 1,
      createdAt: '2026-11-01T12:00:00Z',
      updatedAt: '2026-11-01T12:00:00Z',
      requests: 1,
      held: false,
      unwanted: false,
      stopPending: false,
      stopHeld: false,
      journey: NO_JOURNEY,
      serviceName: 'Radarr',
      ...patch,
    }) as Acquisition

  it('narrows to what needs attention, in the URL', async () => {
    const list = [
      acquisition(1, {}),
      acquisition(2, { state: 'failed', lastError: 'HTTP 400' }),
      acquisition(3, { state: 'pending', held: true }),
      acquisition(4, { sourceMessage: 'No files found are eligible for import' }),
    ]
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(ok(path === '/api/v1/admin/acquisitions' ? list : null)),
    )
    page('/admin/downloads')
    expect(await screen.findByText('4 entries')).toBeTruthy()
    fireEvent.click(screen.getByRole('radio', { name: 'Needs attention' }))
    await waitFor(() => expect(screen.getByText('3 entries')).toBeTruthy())
    expect(where()).toContain('show=attention')
    fireEvent.click(screen.getByRole('radio', { name: 'Stuck' }))
    await waitFor(() => expect(screen.getByText('1 entry')).toBeTruthy())
  })
})

describe('PosterThumb (PRIV-20)', () => {
  it('builds a private image from the private catalog, never TMDB', () => {
    renderWithProviders(<PosterThumb file="0f1e2d3c" title="x" category="adult" />, {
      router: false,
    })
    const img = document.querySelector('img') as HTMLImageElement
    expect(img.getAttribute('src')).toBe('/api/v1/images/stashdb/300/0f1e2d3c')
  })

  it('shows a lock for a neutral private row', () => {
    renderWithProviders(<PosterThumb file={null} title="" category="adult" />, { router: false })
    expect(document.querySelector('img')).toBeNull()
    expect(document.querySelector('svg')).not.toBeNull()
  })
})
