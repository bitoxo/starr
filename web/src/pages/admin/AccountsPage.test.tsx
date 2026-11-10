import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { fail, ok, renderWithProviders } from '@/test/render'

import AccountsPage from './AccountsPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const SERVICE = {
  id: 2,
  name: 'Kino',
  adapterType: 'jellyfin',
  baseUrl: 'http://jf',
  mode: 'live',
  capabilities: ['accounts'],
  createdAt: '2026-11-01T12:00:00Z',
  updatedAt: '2026-11-01T12:00:00Z',
}

function cell(overrides: Record<string, unknown>) {
  return { serviceId: 2, decision: 'undecided', state: 'undecided', steps: [], ...overrides }
}

const MATRIX = {
  services: [{ id: 2, name: 'Kino', adapterType: 'jellyfin', scanComplete: true }],
  members: [
    {
      id: 7,
      username: 'alex',
      displayName: 'Alex',
      status: 'active',
      cells: [cell({ suggestion: { remoteId: 'r-alex', username: 'Alex' } })],
    },
    {
      id: 8,
      username: 'sam',
      displayName: 'Sam',
      status: 'active',
      cells: [
        cell({
          decision: 'linked',
          state: 'managed',
          account: { remoteId: 'r-friend', username: 'friend' },
          origin: 'adopted',
          password: 'deferred',
          linkedUsername: 'friend',
          steps: [
            {
              action: 'setLibraries',
              reason: 'librariesDiffer',
              destructive: true,
              fromAll: true,
              add: [{ id: 'm', name: 'Movies' }],
              remove: [],
            },
          ],
        }),
      ],
    },
  ],
}

const WRITES = { mode: 'planOnly', next: 'planOnly', cap: 5, approved: false }

const DISABLE = {
  action: 'disable',
  reason: 'suspended',
  destructive: true,
  fromAll: false,
  add: [],
  remove: [],
}

const PLAN = {
  serviceId: 2,
  writes: WRITES,
  computedAt: '2026-11-01T12:00:00Z',
  complete: true,
  fingerprint: 'abc',
  adminsWithoutTotp: [] as string[],
  counts: {
    create: 1,
    enable: 0,
    disable: 0,
    libraries: 1,
    destructive: 1,
    undecided: 1,
    conflicts: 0,
    waiting: 0,
  },
  entries: [
    {
      member: { id: 9, username: 'mia' },
      state: 'toCreate',
      steps: [
        {
          action: 'create',
          reason: 'entitled',
          destructive: false,
          fromAll: false,
          username: 'mia',
          add: [{ id: 'm', name: 'Movies' }],
          remove: [],
        },
      ],
    },
    { member: { id: 10, username: 'kim' }, state: 'managed', steps: [] },
  ],
}

/** A plan with `n` disable steps (one member each). */
function planWith(n: number, writes: Record<string, unknown> = WRITES) {
  return {
    ...PLAN,
    writes,
    counts: { ...PLAN.counts, create: 0, libraries: 0, disable: n, destructive: n },
    entries: Array.from({ length: n }, (_, i) => ({
      member: { id: 20 + i, username: `m${i + 1}` },
      state: 'managed',
      steps: [DISABLE],
    })),
  }
}

let plan: unknown = PLAN

function routes(path: string) {
  switch (path) {
    case '/api/v1/services':
      return ok([SERVICE])
    case '/api/v1/service-types':
      return ok([{ typeId: 'jellyfin', capabilities: ['accounts', 'libraries'] }])
    case '/api/v1/categories':
      return ok([{ category: 'movies', consumers: [{ id: 4, serviceId: 2 }] }])
    case '/api/v1/accounts/matrix':
      return ok(MATRIX)
    case '/api/v1/services/{id}/account-plan':
      return ok(plan)
    case '/api/v1/auth/me':
      return ok({ kind: 'admin', id: 1, username: 'root', displayName: 'Root', totpEnabled: false })
    case '/api/v1/services/{id}/accounts':
      return ok({ serviceId: 2, accounts: [] })
    default:
      throw new Error(`unexpected GET ${path}`)
  }
}

describe('Accounts', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    api.PUT.mockReset()
    api.GET.mockImplementation((path: string) => Promise.resolve(routes(path)))
    plan = PLAN
  })

  it('shows who gets which account, with suggestions and planned steps', async () => {
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts' })
    const table = await screen.findByRole('table')
    const alex = within(table).getByRole('link', { name: 'Alex' }).closest('tr') as HTMLElement
    expect(within(alex).getByText('undecided')).toBeTruthy()
    expect(within(alex).getByText('suggests “Alex”')).toBeTruthy()
    const sam = within(table).getByRole('link', { name: 'Sam' }).closest('tr') as HTMLElement
    expect(within(sam).getByText('managed')).toBeTruthy()
    expect(within(sam).getByText('named “friend” there')).toBeTruthy()
    expect(within(sam).getByText('uses old password')).toBeTruthy()
    expect(within(sam).getByText('1 planned step')).toBeTruthy()
    // Only rows with a suggestion offer the one-click confirmation.
    expect(within(sam).queryByRole('button', { name: /Confirm the suggested accounts/ })).toBeNull()
  })

  it('says how a server without an off switch disables, and which accounts Starr locked', async () => {
    const matrix = {
      services: [
        {
          id: 3,
          name: 'Navidrome',
          adapterType: 'navidrome',
          scanComplete: true,
          disablesByPassword: true,
        },
      ],
      members: [
        {
          id: 8,
          username: 'sam',
          displayName: 'Sam',
          status: 'suspended',
          cells: [
            cell({
              serviceId: 3,
              decision: 'linked',
              state: 'managed',
              account: { remoteId: 'nd-sam', username: 'sam' },
              origin: 'adopted',
              password: 'deferred',
              linkedUsername: 'sam',
              lockedByStarr: true,
            }),
          ],
        },
        {
          id: 9,
          username: 'mia',
          displayName: 'Mia',
          status: 'active',
          cells: [
            cell({
              serviceId: 3,
              decision: 'create',
              state: 'managed',
              account: { remoteId: 'nd-mia', username: 'mia' },
              origin: 'provisioned',
              password: 'starr',
              linkedUsername: 'mia',
              lockedByStarr: false,
            }),
          ],
        },
      ],
    }
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(path === '/api/v1/accounts/matrix' ? ok(matrix) : routes(path)),
    )
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts' })
    expect(await screen.findByText(/Navidrome has no off switch for accounts/)).toBeTruthy()
    const table = await screen.findByRole('table')
    expect(within(table).getByText('no off switch')).toBeTruthy()
    const sam = within(table).getByRole('link', { name: 'Sam' }).closest('tr') as HTMLElement
    expect(within(sam).getByText(/locked with a random password/)).toBeTruthy()
    // The old password no longer works there.
    expect(within(sam).queryByText('uses old password')).toBeNull()
    const mia = within(table).getByRole('link', { name: 'Mia' }).closest('tr') as HTMLElement
    expect(within(mia).queryByText(/locked with a random password/)).toBeNull()
  })

  it('confirms the suggestions of a row in one click', async () => {
    api.POST.mockResolvedValue(ok(MATRIX.members[0]))
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts' })
    const table = await screen.findByRole('table')
    fireEvent.click(
      within(table).getByRole('button', { name: 'Confirm the suggested accounts of alex' }),
    )
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith(
        '/api/v1/members/{id}/service-accounts/confirm-suggestions',
        { params: { path: { id: 7 } } },
      ),
    )
  })

  it('shows the plan of one server as plan only', async () => {
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts?service=2' })
    expect(await screen.findByText(/Nothing is written to Kino/)).toBeTruthy()
    expect(screen.getByText('Plan only')).toBeTruthy()
    expect(screen.getByText('Create “mia”')).toBeTruthy()
    expect(screen.getByText('+ Movies')).toBeTruthy()
    expect(screen.getByText('member is active and entitled')).toBeTruthy()
    // kim is in sync: summarised, not listed.
    expect(screen.queryByRole('link', { name: 'kim' })).toBeNull()
    expect(screen.getByText('1 more member needs nothing here.')).toBeTruthy()
  })

  it('decides for one cell from its menu', async () => {
    api.PUT.mockResolvedValue(ok(MATRIX.members[0].cells[0]))
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts' })
    const table = await screen.findByRole('table')
    const trigger = within(table).getByRole('button', { name: 'Decide for alex in Kino' })
    fireEvent.keyDown(trigger, { key: 'Enter' })
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Create a new account' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/members/{id}/service-accounts/{serviceId}', {
        params: { path: { id: 7, serviceId: 2 } },
        body: { decision: 'create', remoteId: null },
      }),
    )
  })

  it('asks for the password before the link dialog, not on top of it (ADM-34)', async () => {
    renderWithProviders(<AccountsPage />, {
      path: '/admin/media/accounts',
      me: { kind: 'admin', username: 'root', displayName: 'Root', totpEnabled: false },
    })
    const table = await screen.findByRole('table')
    const trigger = within(table).getByRole('button', { name: 'Decide for alex in Kino' })
    fireEvent.keyDown(trigger, { key: 'Enter' })
    fireEvent.click(await screen.findByRole('menuitem', { name: /Link another account/ }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByLabelText('Admin password')).toBeTruthy()
    // Only the re-authentication is open; the link dialog waits for it.
    expect(screen.getAllByRole('dialog')).toHaveLength(1)
  })

  it('opens the link dialog at once while elevated and says until when', async () => {
    const until = new Date(Date.now() + 4 * 60_000).toISOString()
    renderWithProviders(<AccountsPage />, {
      path: '/admin/media/accounts',
      me: { kind: 'admin', username: 'root', displayName: 'Root', elevatedUntil: until },
    })
    const table = await screen.findByRole('table')
    fireEvent.keyDown(within(table).getByRole('button', { name: 'Decide for alex in Kino' }), {
      key: 'Enter',
    })
    fireEvent.click(await screen.findByRole('menuitem', { name: /Link another account/ }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByText(/Confirmed until/)).toBeTruthy()
    expect(within(dialog).queryByLabelText('Admin password')).toBeNull()
  })

  it('keeps the member column in view and gives the scan button no gold (ADM-27, ADM-16)', async () => {
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts' })
    const table = await screen.findByRole('table')
    const header = within(table).getAllByRole('columnheader')[0]
    expect(header.className).toContain('sticky')
    expect(screen.getByRole('button', { name: /Scan now/ }).className).not.toContain('polaris')
  })

  it('goes live only after confirming the plan shown, its first five steps, and the password', async () => {
    plan = planWith(7)
    const live = planWith(7, { ...WRITES, mode: 'live', next: 'execute', approved: true })
    api.POST.mockResolvedValueOnce(fail('auth.elevation_required', 403))
      .mockResolvedValueOnce(ok({ elevatedUntil: '2026-11-03T12:05:00Z' }))
      .mockResolvedValueOnce(ok(live))
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts?service=2' })
    fireEvent.click(await screen.findByRole('button', { name: 'Go live…' }))
    const dialog = await screen.findByRole('dialog', { name: 'Go live in Kino?' })
    // The first five steps by member, then how many more.
    for (const name of ['m1', 'm2', 'm3', 'm4', 'm5']) {
      expect(within(dialog).getByText(name)).toBeTruthy()
    }
    expect(within(dialog).queryByText('m6')).toBeNull()
    expect(within(dialog).getByText('and 2 more steps')).toBeTruthy()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Go live' }))
    // The server wants a fresh elevation: ask for the password, then retry.
    const elevation = await screen.findByRole('dialog', { name: "Confirm it's you" })
    fireEvent.change(within(elevation).getByLabelText('Admin password'), {
      target: { value: 'correct horse battery staple' },
    })
    fireEvent.click(within(elevation).getByRole('button', { name: 'Confirm' }))
    await waitFor(() => expect(api.POST).toHaveBeenCalledTimes(3))
    expect(api.POST).toHaveBeenNthCalledWith(1, '/api/v1/services/{id}/account-plan/confirm', {
      params: { path: { id: 2 } },
      body: { fingerprint: 'abc' },
    })
    expect(api.POST).toHaveBeenNthCalledWith(2, '/api/v1/auth/elevate', {
      body: { password: 'correct horse battery staple' },
    })
    expect(api.POST).toHaveBeenNthCalledWith(3, '/api/v1/services/{id}/account-plan/confirm', {
      params: { path: { id: 2 } },
      body: { fingerprint: 'abc' },
    })
  })

  it('asks for two-factor authentication first and leads there (M10 1)', async () => {
    plan = { ...planWith(1), adminsWithoutTotp: ['root', 'ops'] }
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts?service=2' })
    const goLive = await screen.findByRole('button', { name: 'Go live…' })
    expect((goLive as HTMLButtonElement).disabled).toBe(true)
    expect(screen.getByText('Two-factor authentication first')).toBeTruthy()
    expect(screen.getByText(/Still without it: ops\./)).toBeTruthy()
    expect(
      screen.getByRole('link', { name: 'Set up two-factor authentication' }).getAttribute('href'),
    ).toBe('/admin/system/security')
  })

  it('cannot go live on an incomplete plan and explains a held one', async () => {
    plan = { ...planWith(1), complete: false }
    const { unmount } = renderWithProviders(<AccountsPage />, {
      path: '/admin/media/accounts?service=2',
    })
    const goLive = await screen.findByRole('button', { name: 'Go live…' })
    expect((goLive as HTMLButtonElement).disabled).toBe(true)
    unmount()
    plan = planWith(6, {
      ...WRITES,
      mode: 'live',
      next: 'held',
      holdReason: 'overCap',
      liveSince: '2026-11-01T12:00:00Z',
      liveBy: 'root',
    })
    renderWithProviders(<AccountsPage />, { path: '/admin/media/accounts?service=2' })
    expect(
      await screen.findByText(/6 steps disable accounts or take libraries away, more than 5/),
    ).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Approve…' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Pause writes' })).toBeTruthy()
  })
})
