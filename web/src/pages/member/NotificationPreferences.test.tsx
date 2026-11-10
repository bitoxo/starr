import { fireEvent, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { NotificationPreferences as Prefs } from '@/api/notifications'
import { ok, renderWithProviders } from '@/test/render'

import { NotificationPreferences } from './NotificationPreferences'

const api = vi.hoisted(() => ({ GET: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const REQUEST_KINDS = [
  'account_activate',
  'request_approved',
  'request_rejected',
  'request_revoked',
  'request_partially_available',
  'request_available',
  'request_failed',
] as const

function prefs(over: Partial<Prefs>): Prefs {
  return {
    kinds: [...REQUEST_KINDS],
    follows: [],
    email: [],
    emailAvailable: true,
    emailAddress: 'ben@example.org',
    ...over,
  }
}

describe('notification preferences', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.PUT.mockReset()
  })

  it('lists only the kinds the server offers: nothing of follows or the weekly cap (PRV-01)', async () => {
    api.GET.mockResolvedValue(ok(prefs({})))
    renderWithProviders(<NotificationPreferences />)
    expect(await screen.findByText('Ready to watch')).toBeTruthy()
    expect(screen.getAllByRole('switch')).toHaveLength(REQUEST_KINDS.length)
    expect(document.body.textContent).not.toMatch(/scene|performer|studio|weekly|follow/i)
  })

  it('names only what the member can follow', async () => {
    api.GET.mockResolvedValue(
      ok(prefs({ kinds: ['follow_requested', ...REQUEST_KINDS], follows: ['series'] })),
    )
    renderWithProviders(<NotificationPreferences />)
    expect(await screen.findByText('New season you follow')).toBeTruthy()
    expect(document.body.textContent).not.toMatch(/album|artist/i)
  })

  it('saves the offered kinds in display order', async () => {
    api.GET.mockResolvedValue(ok(prefs({ email: ['request_available'] })))
    api.PUT.mockResolvedValue(ok(prefs({ email: ['request_approved', 'request_available'] })))
    renderWithProviders(<NotificationPreferences />)
    fireEvent.click(await screen.findByRole('switch', { name: 'Email for: Approved' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/me/notification-preferences', {
        body: { email: ['request_approved', 'request_available'] },
      }),
    )
  })

  it('explains why email is off instead of showing dead switches (MEM-28)', async () => {
    api.GET.mockResolvedValue(ok(prefs({ emailAvailable: false })))
    renderWithProviders(<NotificationPreferences />)
    expect(await screen.findByText('Ready to watch')).toBeTruthy()
    expect(screen.queryAllByRole('switch')).toHaveLength(0)
    expect(screen.getByText(/Email isn't set up on this server yet/)).toBeTruthy()
    expect(screen.getByText(/Ask your admin to set up email/)).toBeTruthy()
    // Each kind says where it shows up now.
    expect(screen.getAllByText('In the bell')).toHaveLength(REQUEST_KINDS.length)
  })

  it('names the missing address as the next step', async () => {
    api.GET.mockResolvedValue(ok(prefs({ emailAddress: null })))
    renderWithProviders(<NotificationPreferences />)
    expect(await screen.findByText(/Your account has no email address/)).toBeTruthy()
    expect(screen.queryAllByRole('switch')).toHaveLength(0)
  })
})
