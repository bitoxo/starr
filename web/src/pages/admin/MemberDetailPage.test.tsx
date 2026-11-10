import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ok, renderWithProviders } from '@/test/render'

import MemberDetailPage from './MemberDetailPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const ENTITLEMENTS = [
  { category: 'movies', enabled: true, requestMode: 'approval' },
  { category: 'series', enabled: false, requestMode: 'approval' },
  { category: 'music', enabled: false, requestMode: 'approval' },
  { category: 'adult', enabled: false, requestMode: 'approval' },
]

const BEN = {
  id: 4,
  username: 'ben',
  displayName: 'Ben',
  email: 'ben@example.org',
  status: 'active',
  passwordSet: true,
  createdAt: '2026-11-01T08:00:00Z',
  entitlements: ENTITLEMENTS,
  sessions: [],
  adultSince: null,
}

function show(member: object) {
  api.GET.mockImplementation((path: string) =>
    Promise.resolve(path === '/api/v1/members/{id}' ? ok(member) : ok([])),
  )
  renderWithProviders(
    <Routes>
      <Route path="/admin/people/members/:id" element={<MemberDetailPage />} />
    </Routes>,
    { path: '/admin/people/members/4' },
  )
}

describe('Member detail', () => {
  beforeEach(() => vi.resetAllMocks())

  it('asks in operator-statement words before switching NSFW on (ADM-12)', async () => {
    show(BEN)
    const save = await screen.findByRole('button', { name: 'Save entitlements' })
    expect(save).toHaveProperty('disabled', true)
    fireEvent.click(screen.getByRole('switch', { name: 'NSFW enabled' }))
    const dialog = await screen.findByRole('alertdialog', { name: 'Switch NSFW on for Ben?' })
    expect(dialog.textContent).toMatch(/only for adults who want it/)
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect(screen.getByRole('switch', { name: 'NSFW enabled' }).getAttribute('aria-checked')).toBe(
      'false',
    )
    expect(save).toHaveProperty('disabled', true)

    fireEvent.click(screen.getByRole('switch', { name: 'NSFW enabled' }))
    fireEvent.click(
      within(await screen.findByRole('alertdialog')).getByRole('button', {
        name: 'Switch on for this member',
      }),
    )
    await waitFor(() => expect(save).toHaveProperty('disabled', false))
  })

  it('says since when NSFW is on', async () => {
    const on = ENTITLEMENTS.map((e) => (e.category === 'adult' ? { ...e, enabled: true } : e))
    show({ ...BEN, entitlements: on, adultSince: '2026-11-03T10:00:00Z' })
    expect(await screen.findByText('On since Nov 3, 2026.')).toBeTruthy()
  })

  it('confirms before suspending (ADM-07)', async () => {
    api.POST.mockResolvedValue(ok({ ...BEN, status: 'suspended' }))
    show(BEN)
    fireEvent.click(await screen.findByRole('button', { name: 'Suspend' }))
    expect(api.POST).not.toHaveBeenCalled()
    const dialog = await screen.findByRole('alertdialog', { name: 'Suspend Ben?' })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Suspend' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/members/{id}/suspend', {
        params: { path: { id: 4 } },
      }),
    )
  })

  it('describes reactivation for a suspended member, not suspension (ADM-13)', async () => {
    show({ ...BEN, status: 'suspended' })
    expect(await screen.findByRole('button', { name: 'Reactivate' })).toBeTruthy()
    expect(screen.getByText(/Lets them sign in again/)).toBeTruthy()
    expect(screen.queryByText(/Signs them out everywhere/)).toBeNull()
  })
})
