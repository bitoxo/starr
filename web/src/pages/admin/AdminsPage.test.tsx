import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { ok, renderWithProviders } from '@/test/render'

import AdminsPage from './AdminsPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

/** Signed-in admin, elevated for another hour unless `elevatedUntil` says otherwise. */
const me = (elevatedUntil: string | null = new Date(Date.now() + 3_600_000).toISOString()) => ({
  kind: 'admin',
  username: 'lena',
  displayName: 'Lena',
  totpEnabled: false,
  elevatedUntil,
})

describe('Suspend admin (ADM-07)', () => {
  it('asks before suspending another admin, never offers it for oneself', async () => {
    const admin = (id: number, current: boolean) => ({
      id,
      username: current ? 'lena' : 'ops',
      displayName: current ? 'Lena' : 'Ops',
      status: 'active',
      totpEnabled: true,
      current,
      createdAt: '2026-11-01T08:00:00Z',
    })
    api.GET.mockResolvedValue(ok([admin(1, true), admin(2, false)]))
    api.POST.mockResolvedValue(ok({ ...admin(2, false), status: 'suspended' }))
    renderWithProviders(<AdminsPage />, { path: '/admin/people/admins', me: me() })
    const buttons = await screen.findAllByRole('button', { name: 'Suspend' })
    expect(buttons).toHaveLength(1)
    fireEvent.click(buttons[0])
    expect(api.POST).not.toHaveBeenCalled()
    const dialog = await screen.findByRole('alertdialog', { name: 'Suspend Ops?' })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Suspend' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admins/{id}/suspend', {
        params: { path: { id: 2 } },
      }),
    )
  })
})

describe('Add admin (ADM-06)', () => {
  it('warns that a new admin without two-step verification pauses account writes', async () => {
    api.GET.mockResolvedValue(ok([]))
    renderWithProviders(<AdminsPage />, { path: '/admin/people/admins', me: me() })
    fireEvent.click(await screen.findByRole('button', { name: 'Add admin' }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByRole('note').textContent).toMatch(
      /live account writes to media servers pause by themselves/,
    )
  })
})

describe('Admin changes need a fresh re-authentication (L3)', () => {
  it('asks for the password before the add dialog opens', async () => {
    api.GET.mockResolvedValue(ok([]))
    api.POST.mockReset()
    api.POST.mockResolvedValue(ok({ elevatedUntil: '2099-01-01T00:00:00Z' }))
    renderWithProviders(<AdminsPage />, { path: '/admin/people/admins', me: me(null) })
    fireEvent.click(await screen.findByRole('button', { name: 'Add admin' }))
    const elevation = await screen.findByRole('dialog', { name: "Confirm it's you" })
    expect(screen.queryByRole('dialog', { name: 'Add admin' })).toBeNull()
    fireEvent.change(within(elevation).getByLabelText('Admin password'), {
      target: { value: 'correct horse battery staple' },
    })
    fireEvent.click(within(elevation).getByRole('button', { name: 'Confirm' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/auth/elevate', expect.anything()),
    )
    expect(await screen.findByRole('note')).toBeTruthy()
  })
})
