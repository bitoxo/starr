import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ok, renderWithProviders } from '@/test/render'

import InvitationsPage from './InvitationsPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

function invitation(id: number, extra: object = {}) {
  return {
    id,
    state: 'pending',
    createdAt: `2026-11-0${id}T08:00:00Z`,
    createdBy: 'lena',
    expiresAt: '2026-11-14T08:00:00Z',
    presetEntitlements: [],
    presetAccounts: [],
    ...extra,
  }
}

describe('Invitations (ADM-14, ADM-07)', () => {
  beforeEach(() => vi.resetAllMocks())

  it('tells invitations apart by created date and hides the expiry once revoked', async () => {
    api.GET.mockResolvedValue(
      ok([
        invitation(1, { state: 'revoked', revokedAt: '2026-11-02T09:00:00Z' }),
        invitation(3, { presetEmail: 'dora@example.org' }),
      ]),
    )
    renderWithProviders(<InvitationsPage />, { path: '/admin/people/invitations' })
    const revoked = (await screen.findByText('Revoked Nov 2, 2026')).closest('tr') as HTMLElement
    expect(within(revoked).getByText('Nov 1, 2026')).toBeTruthy()
    expect(within(revoked).queryByRole('button', { name: /Revoke/ })).toBeNull()
    const open = screen.getByText('dora@example.org').closest('tr') as HTMLElement
    expect(within(open).getByText('Nov 3, 2026')).toBeTruthy()
  })

  it('asks before revoking', async () => {
    api.GET.mockResolvedValue(ok([invitation(3, { presetUsername: 'dora' })]))
    api.POST.mockResolvedValue(ok(invitation(3, { state: 'revoked' })))
    renderWithProviders(<InvitationsPage />, { path: '/admin/people/invitations' })
    fireEvent.click(await screen.findByRole('button', { name: 'Revoke the invitation for dora' }))
    expect(api.POST).not.toHaveBeenCalled()
    const dialog = await screen.findByRole('alertdialog')
    fireEvent.click(within(dialog).getByRole('button', { name: 'Revoke' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/invitations/{id}/revoke', {
        params: { path: { id: 3 } },
      }),
    )
  })
})
