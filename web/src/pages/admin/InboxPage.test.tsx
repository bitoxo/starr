import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { AdminAttention, AttentionItem } from '@/api/attention'
import type { AdminRequest } from '@/api/requests'
import { expectFocusNotOnBody } from '@/test/focus'
import { NO_JOURNEY, ok, renderWithProviders } from '@/test/render'

import InboxPage from './InboxPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))
const toast = vi.hoisted(() =>
  Object.assign(vi.fn(), { error: vi.fn(), dismiss: vi.fn(), custom: vi.fn() }),
)
vi.mock('sonner', () => ({ toast }))
const undo = vi.hoisted(() => ({ showUndoToast: vi.fn() }))
vi.mock('@/lib/undo', () => undo)

function request(id: number, memberId: number, title: string, seasons: number[]): AdminRequest {
  return {
    id,
    member: { id: memberId, username: `member${memberId}`, displayName: `Member ${memberId}` },
    redacted: false,
    kind: seasons.length ? 'series' : 'movie',
    titleId: String(id),
    title,
    year: 2010,
    poster: null,
    category: seasons.length ? 'series' : 'movies',
    state: 'pending_approval',
    rejectReason: null,
    follow: false,
    hidden: false,
    canRevoke: true,
    createdAt: '2026-11-01T12:00:00Z',
    updatedAt: '2026-11-01T12:00:00Z',
    items: (seasons.length ? seasons : [null]).map((season, i) => ({
      id: id * 10 + i,
      season,
      decision: 'pending_approval',
      state: 'pending_approval',
      journey: NO_JOURNEY,
    })),
  }
}

function approval(r: AdminRequest): AttentionItem {
  return {
    id: `approval:${r.id}`,
    kind: 'approval',
    section: 'decide',
    since: r.createdAt,
    redacted: r.redacted,
    request: r,
  }
}

const HIDDEN: AdminRequest = {
  ...request(9, 3, '', []),
  redacted: true,
  category: 'adult',
  titleId: null,
  year: null,
  member: { id: 3, username: '', displayName: '' },
}

const HELD: AttentionItem = {
  id: 'held:4',
  kind: 'held',
  section: 'paused',
  since: '2026-11-04T10:00:00Z',
  redacted: false,
  writes: {
    serviceId: 4,
    serviceName: 'Lidarr',
    adapterType: 'lidarr',
    kind: 'source',
    pausedAt: '2026-11-04T10:00:00Z',
    pausedBy: 'root',
    automatic: false,
    adminsWithoutTotp: [],
    canResume: true,
    held: {
      total: 2,
      preview: [
        { title: 'Mira Okafor', album: 'Sundial', redacted: false },
        { title: 'The Velvet Index', album: 'Neon Psalms', redacted: false },
      ],
      stopTotal: 0,
      stops: [],
    },
  },
}

const CONFLICT: AttentionItem = {
  id: 'conflict:2:8',
  kind: 'account_conflict',
  section: 'fix',
  redacted: false,
  conflict: {
    serviceId: 2,
    serviceName: 'Navidrome',
    memberId: 8,
    username: 'ben',
    kind: 'nameTaken',
    remoteId: 'r-ben',
    remoteName: 'ben',
  },
}

const SETUP: AttentionItem = {
  id: 'setup',
  kind: 'setup',
  section: 'setup',
  redacted: false,
  setup: [
    { step: 'services', done: true },
    { step: 'categories', done: true },
    { step: 'two_step', done: false },
    { step: 'members', done: true },
  ],
}

function attention(items: AttentionItem[]): AdminAttention {
  const n = (s: AttentionItem['section']) => items.filter((i) => i.section === s).length
  return {
    total: items.length,
    sections: { decide: n('decide'), fix: n('fix'), paused: n('paused'), setup: n('setup') },
    items,
  }
}

const INCEPTION = request(1, 7, 'Inception', [])
const THRONES = request(2, 8, 'Game of Thrones', [1, 2])

function serve(items: AttentionItem[]) {
  let current = attention(items)
  api.GET.mockImplementation((path: string) => {
    if (path === '/api/v1/admin/attention') return Promise.resolve(ok(current))
    if (path === '/api/v1/categories') return Promise.resolve(ok([]))
    return Promise.resolve(ok([]))
  })
  return {
    set(next: AttentionItem[]) {
      current = attention(next)
    },
  }
}

const decide = () => screen.getByRole('region', { name: /^Decide/ })

describe('Inbox (spec 0008 F9, Q9)', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    api.PUT.mockReset()
    toast.mockReset()
    undo.showUndoToast.mockReset()
  })

  it('shows the sections with one count everywhere and folds empty ones (ADM-03, ADM-23)', async () => {
    serve([approval(INCEPTION), approval(THRONES), CONFLICT, HELD, SETUP])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    expect(await screen.findByText('5 things need you')).toBeTruthy()
    const counts = ['Decide', 'Fix', 'Paused', 'Setup'].map((name) => {
      const section = screen.getByRole('region', { name: new RegExp(`^${name}`) })
      const counter = within(section).getByRole('heading').querySelector('[aria-hidden]')
      expect(within(section).getByRole('heading').textContent).toMatch(/ items?$/)
      return Number(counter?.textContent)
    })
    expect(counts).toEqual([2, 1, 1, 1])
    expect(counts.reduce((a, b) => a + b)).toBe(5)
    expect(within(decide()).getByText('Inception')).toBeTruthy()
    expect(within(decide()).getByText('S01, S02 · 2010')).toBeTruthy()
    // No gold in rows (ADM-16) and no focusable decorative chips (ADM-38).
    expect(document.querySelector('.bg-polaris')).toBeNull()
    expect(within(decide()).queryByRole('button', { name: /Waiting for approval/ })).toBeNull()
  })

  it('folds an empty section into one line', async () => {
    serve([SETUP])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const fix = await screen.findByRole('region', { name: 'Fix' })
    expect(within(fix).getByText('Nothing waiting')).toBeTruthy()
    expect(within(fix).queryByRole('list')).toBeNull()
  })

  it('says all clear when nothing waits', async () => {
    serve([])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    expect(await screen.findByText('All clear')).toBeTruthy()
    expect(screen.getByText('Nothing needs you right now')).toBeTruthy()
  })

  it('approves in the row with the outcome and undo, and focus moves to the next row (ADM-30, F-4)', async () => {
    const inbox = serve([approval(INCEPTION), approval(THRONES)])
    api.POST.mockResolvedValue(
      ok({ decided: [1], skipped: [], undoable: true, held: [{ requestId: 1, source: 'Radarr' }] }),
    )
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const button = await screen.findByRole('button', { name: 'Approve Inception' })
    button.focus()
    inbox.set([approval(THRONES)])
    fireEvent.click(button)
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/approvals/approve', {
        body: { requests: [{ id: 1, memberId: 7 }] },
      }),
    )
    await waitFor(() => expect(screen.getByText('1 thing needs you')).toBeTruthy())
    expect(screen.queryByText('Inception')).toBeNull()
    const [shown] = undo.showUndoToast.mock.calls[0] as [{ message: string; onUndo: () => void }]
    expect(shown.message).toBe('Approved: Inception – held until Radarr resumes')
    expectFocusNotOnBody()
    expect(document.activeElement?.closest('[data-item="approval:2"]')).not.toBeNull()

    api.POST.mockResolvedValue(ok({ decided: [1], skipped: [], undoable: false, held: [] }))
    await act(async () => shown.onUndo())
    expect(api.POST).toHaveBeenLastCalledWith('/api/v1/admin/approvals/undo', {
      body: { requests: [{ id: 1, memberId: 7 }] },
    })
  })

  it('offers no undo once the approval went to the source', async () => {
    serve([approval(INCEPTION)])
    api.POST.mockResolvedValue(ok({ decided: [1], skipped: [], undoable: false, held: [] }))
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    fireEvent.click(await screen.findByRole('button', { name: 'Approve Inception' }))
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith('Approved: Inception', { id: undefined }),
    )
    expect(undo.showUndoToast).not.toHaveBeenCalled()
  })

  it('declines with a required reason in the row, without undo (ADM-36, D11)', async () => {
    serve([approval(INCEPTION)])
    api.POST.mockResolvedValue(ok({ decided: [1], skipped: [], undoable: false, held: [] }))
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    fireEvent.click(await screen.findByRole('button', { name: 'Decline Inception' }))
    const field = screen.getByRole('textbox', { name: /Reason/ })
    expect(screen.getByText('(required)')).toBeTruthy()
    expect(document.activeElement).toBe(field)
    fireEvent.click(screen.getByRole('button', { name: 'Decline request' }))
    expect(screen.getByText('Add a reason. The member reads it.')).toBeTruthy()
    expect(field.getAttribute('aria-invalid')).toBe('true')
    expect(api.POST).not.toHaveBeenCalled()
    fireEvent.change(field, { target: { value: 'Not on this server' } })
    fireEvent.click(screen.getByRole('button', { name: 'Decline request' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/approvals/reject', {
        body: { requests: [{ id: 1, memberId: 7 }], reason: 'Not on this server' },
      }),
    )
    await waitFor(() => expect(toast).toHaveBeenCalledWith('Declined: Inception'))
    expect(undo.showUndoToast).not.toHaveBeenCalled()
  })

  it('Esc closes the decline step and gives focus back', async () => {
    serve([approval(INCEPTION)])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    fireEvent.click(await screen.findByRole('button', { name: 'Decline Inception' }))
    fireEvent.keyDown(screen.getByRole('textbox', { name: /Reason/ }), { key: 'Escape' })
    expect(screen.queryByRole('textbox', { name: /Reason/ })).toBeNull()
    // The row's Decline button is new: focus must land on it, not on <body> (slice 12).
    await waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByRole('button', { name: 'Decline Inception' }),
      ),
    )
    // Cancel does the same.
    fireEvent.click(screen.getByRole('button', { name: 'Decline Inception' }))
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    await waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByRole('button', { name: 'Decline Inception' }),
      ),
    )
  })

  it('shows a failed approve at its row and keeps the other rows usable (ADM-32)', async () => {
    serve([approval(INCEPTION), approval(THRONES)])
    api.POST.mockResolvedValue({
      data: undefined,
      error: { code: 'requests.category_not_ready' },
      response: new Response(null, { status: 409 }),
    })
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    fireEvent.click(await screen.findByRole('button', { name: 'Approve Inception' }))
    const row = (await screen.findByRole('alert')).closest('[data-item]')
    expect(row?.getAttribute('data-item')).toBe('approval:1')
    expect(
      (
        screen.getByRole('button', {
          name: 'Approve Game of Thrones S01, S02',
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false)
  })

  it('keeps private rows neutral and unlocks in place with "Show to decide" (PRIV-08)', async () => {
    serve([approval(INCEPTION), approval({ ...HIDDEN, title: 'Private category · request #9' })])
    api.PUT.mockResolvedValue(ok({ unlocked: true, unlockedUntil: '2026-11-07T21:14:00Z' }))
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const row = (await screen.findByText('Private category · request #9')).closest(
      'li',
    ) as HTMLElement
    expect(within(row).getByText('Details stay hidden until you show them.')).toBeTruthy()
    // Never decided blind: no Approve, Decline or checkbox on the neutral row.
    expect(within(row).queryByRole('button', { name: /Approve|Decline/ })).toBeNull()
    expect(within(row).queryByRole('checkbox')).toBeNull()
    fireEvent.click(
      within(row).getByRole('button', { name: 'Show the details of private request #9' }),
    )
    const group = within(row).getByRole('group', { name: 'Show private details' })
    fireEvent.click(within(group).getByRole('button', { name: 'Show' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/auth/private', { body: { unlocked: true } }),
    )
  })

  it('focuses the row’s Approve once it names the title, even if that comes first (slice 12)', async () => {
    const shown: AdminRequest = { ...request(9, 3, 'Scene 06', []), category: 'adult' }
    const inbox = serve([approval({ ...HIDDEN, title: 'Private category · request #9' })])
    // The refetch after unlocking answers before the mutation's own callbacks run.
    api.PUT.mockImplementation(() => {
      inbox.set([approval(shown)])
      return Promise.resolve(ok({ unlocked: true, unlockedUntil: '2026-11-07T21:14:00Z' }))
    })
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    fireEvent.click(
      await screen.findByRole('button', { name: 'Show the details of private request #9' }),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Show' }))
    const approve = await screen.findByRole('button', { name: /Approve Scene 06/ })
    await waitFor(() => expect(document.activeElement).toBe(approve))
  })

  it('opens the detail panel from ?open= with exactly one gold action and back again (Q9 C)', async () => {
    serve([approval(INCEPTION), approval(THRONES)])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox?open=approval%3A2' })
    const heading = await screen.findByRole('heading', { name: 'Game of Thrones', level: 2 })
    expect(document.activeElement).toBe(heading)
    const panel = screen.getByRole('region', { name: 'Game of Thrones' })
    expect(panel.querySelectorAll('.bg-polaris')).toHaveLength(1)
    expect(
      within(panel).getByRole('button', { name: 'Approve Game of Thrones S01, S02' }),
    ).toBeTruthy()
    // Below 1280 px the panel replaces the sections.
    expect(screen.queryByRole('region', { name: /^Decide/ })).toBeNull()
    fireEvent.click(within(panel).getByRole('button', { name: 'Back to inbox' }))
    await waitFor(() =>
      expect(document.activeElement?.getAttribute('aria-label')).toBe(
        'Open details of Game of Thrones',
      ),
    )
  })

  it('says so when ?open= names something already done', async () => {
    serve([approval(INCEPTION)])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox?open=approval%3A99' })
    expect(await screen.findByText('Nothing waits for this any more.')).toBeTruthy()
    expect(screen.getByText('Inception')).toBeTruthy()
  })

  it('resumes a paused source after a dialog that lists what goes out', async () => {
    serve([HELD])
    api.PUT.mockResolvedValue(ok({ id: 4 }))
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const paused = await screen.findByRole('region', { name: /^Paused/ })
    expect(within(paused).getByText('Lidarr writes paused')).toBeTruthy()
    expect(within(paused).getByText('Mira Okafor – Sundial')).toBeTruthy()
    fireEvent.click(within(paused).getByRole('button', { name: 'Resume Lidarr writes' }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByText('Mira Okafor – Sundial')).toBeTruthy()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Resume and send' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/services/{id}/writes-paused', {
        params: { path: { id: 4 } },
        body: { paused: false },
      }),
    )
  })

  it('says why account writes paused by themselves and when they may resume (ADM-06)', async () => {
    const auto: AttentionItem = {
      id: 'writes:6',
      kind: 'writes_paused',
      section: 'paused',
      redacted: false,
      since: '2026-11-07T08:00:00Z',
      writes: {
        serviceId: 6,
        serviceName: 'Jellyfin',
        adapterType: 'jellyfin',
        kind: 'accounts',
        pausedAt: '2026-11-07T08:00:00Z',
        pausedBy: null,
        automatic: true,
        adminsWithoutTotp: ['lea'],
        canResume: false,
      },
    }
    serve([auto])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const paused = await screen.findByRole('region', { name: /^Paused/ })
    expect(within(paused).getByText('Jellyfin account writes paused')).toBeTruthy()
    expect(within(paused).getByText(/Paused automatically/)).toBeTruthy()
    expect(within(paused).getByText(/until lea turns on two-step/)).toBeTruthy()
    expect(within(paused).queryByRole('button', { name: /Resume/ })).toBeNull()
  })

  it('resolves a taken account name in the row (ADM-08)', async () => {
    serve([CONFLICT])
    api.PUT.mockResolvedValue(ok({}))
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const fix = await screen.findByRole('region', { name: /^Fix/ })
    expect(within(fix).getByText('Navidrome account for ben')).toBeTruthy()
    const link = within(fix).getByRole('radio', { name: /Link ben to the existing account/ })
    expect((link as HTMLInputElement).checked).toBe(true)
    fireEvent.click(within(fix).getByRole('button', { name: 'Apply' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/members/{id}/service-accounts/{serviceId}', {
        params: { path: { id: 8, serviceId: 2 } },
        body: { decision: 'linked', remoteId: 'r-ben' },
      }),
    )
  })

  it('lists the setup checklist with the next step', async () => {
    serve([SETUP])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const setup = await screen.findByRole('region', { name: /^Setup/ })
    expect(within(setup).getByText('3 of 4 done')).toBeTruthy()
    expect(
      within(setup)
        .getByRole('link', { name: 'Next step: Turn on two-step verification for your account' })
        .getAttribute('href'),
    ).toBe('/admin/system/security')
  })

  it('approves a selection from a bar that names its actions (ADM-31)', async () => {
    serve([approval(INCEPTION), approval(THRONES)])
    api.POST.mockResolvedValue(ok({ decided: [1, 2], skipped: [], undoable: false, held: [] }))
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    fireEvent.click(await screen.findByRole('checkbox', { name: 'Select Inception' }))
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select Game of Thrones' }))
    const bar = screen.getByRole('region', { name: '2 selected' })
    expect(bar.className).toContain('sticky')
    expect(within(bar).queryByRole('button', { name: /Revoke/ })).toBeNull()
    fireEvent.click(within(bar).getByRole('button', { name: 'Approve selected' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/approvals/approve', {
        body: {
          requests: [
            { id: 1, memberId: 7 },
            { id: 2, memberId: 8 },
          ],
        },
      }),
    )
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith('2 requests approved', { id: undefined }),
    )
  })

  it('stacks rows as cards with full-width actions on phones (ADM-09)', async () => {
    serve([approval(INCEPTION)])
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    const row = (await screen.findByText('Inception')).closest('li') as HTMLElement
    expect(row.className).toContain('max-md:rounded-panel')
    const actions = row.querySelector('[data-actions]') as HTMLElement
    expect(actions.className).toContain('max-md:grid-cols-2')
    expect(actions.className).toContain('max-md:basis-full')
  })

  it('shows a load error with a retry, never an endless loading line', async () => {
    api.GET.mockResolvedValue({
      data: undefined,
      error: { code: 'unavailable' },
      response: new Response(null, { status: 503 }),
    })
    renderWithProviders(<InboxPage />, { path: '/admin/inbox' })
    expect(await screen.findByRole('button', { name: 'Try again' })).toBeTruthy()
  })
})
