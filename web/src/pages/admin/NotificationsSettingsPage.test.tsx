import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { fail, ok, renderWithProviders } from '@/test/render'

import NotificationsSettingsPage from './NotificationsSettingsPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn(), PATCH: vi.fn(), DELETE: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const EMAIL = {
  id: 1,
  kind: 'email',
  name: 'Email',
  enabled: false,
  hasSecret: true,
  adminEvents: [],
  email: {
    host: 'smtp.example.org',
    port: 587,
    security: 'starttls',
    fromAddress: 'starr@example.org',
  },
  lastOk: false,
  lastAttemptAt: '2026-11-07T08:00:00Z',
  lastErrorCode: 'transient',
  lastError: 'connection refused (os error 61)',
}

const HOOK = {
  id: 2,
  kind: 'discord',
  name: 'Admins on Discord',
  enabled: true,
  hasSecret: true,
  host: 'discord.com',
  adminEvents: ['approval_pending'],
  lastOk: true,
  lastAttemptAt: '2026-11-07T08:00:00Z',
}

describe('Notification channels (ADM-26)', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    api.GET.mockResolvedValue(ok([EMAIL, HOOK]))
  })

  it('shows a failed delivery in words even while the channel is off, raw text collapsed', async () => {
    renderWithProviders(<NotificationsSettingsPage />)
    expect(await screen.findByText(/Starr could not reach the service/)).toBeTruthy()
    expect(screen.queryByText('Off')).toBeNull()
    const raw = screen.getByText('connection refused (os error 61)')
    expect(raw.closest('details')?.open).toBe(false)
  })

  it('names every switch and saves a webhook only with its Save button', async () => {
    api.PATCH.mockResolvedValue(ok(HOOK))
    renderWithProviders(<NotificationsSettingsPage />)
    expect(await screen.findByRole('switch', { name: 'Send emails' })).toBeTruthy()
    const toggle = screen.getByRole('switch', { name: 'Send to Admins on Discord' })
    const row = toggle.closest('li') as HTMLElement
    const save = within(row).getByRole('button', { name: 'Save' })
    expect(save).toHaveProperty('disabled', true)
    fireEvent.click(toggle)
    expect(api.PATCH).not.toHaveBeenCalled()
    expect(save).toHaveProperty('disabled', false)
    fireEvent.click(save)
    await waitFor(() =>
      expect(api.PATCH).toHaveBeenCalledWith('/api/v1/admin/notification-channels/{id}', {
        params: { path: { id: 2 } },
        body: { enabled: false, adminEvents: ['approval_pending'] },
      }),
    )
  })

  it('asks to re-authenticate before a new email server is saved (L3)', async () => {
    api.PATCH.mockResolvedValueOnce(fail('auth.elevation_required', 403)).mockResolvedValueOnce(
      ok(EMAIL),
    )
    api.POST.mockResolvedValue(ok({ elevatedUntil: '2099-01-01T00:00:00Z' }))
    renderWithProviders(<NotificationsSettingsPage />)
    await screen.findByRole('switch', { name: 'Send emails' })
    const card = screen.getByRole('switch', { name: 'Send emails' }).closest('form') as HTMLElement
    fireEvent.change(within(card).getByLabelText('SMTP server'), {
      target: { value: 'smtp.other.example' },
    })
    fireEvent.click(within(card).getByRole('button', { name: 'Save' }))
    const elevation = await screen.findByRole('dialog', { name: "Confirm it's you" })
    fireEvent.change(within(elevation).getByLabelText('Admin password'), {
      target: { value: 'correct horse battery staple' },
    })
    fireEvent.click(within(elevation).getByRole('button', { name: 'Confirm' }))
    await waitFor(() => expect(api.PATCH).toHaveBeenCalledTimes(2))
  })

  it('asks before removing a webhook (ADM-07)', async () => {
    api.DELETE.mockResolvedValue(ok(undefined, 204))
    renderWithProviders(<NotificationsSettingsPage />)
    const toggle = await screen.findByRole('switch', { name: 'Send to Admins on Discord' })
    const row = toggle.closest('li') as HTMLElement
    fireEvent.click(within(row).getByRole('button', { name: 'Remove' }))
    expect(api.DELETE).not.toHaveBeenCalled()
    const dialog = await screen.findByRole('alertdialog', { name: 'Remove Admins on Discord?' })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }))
    await waitFor(() =>
      expect(api.DELETE).toHaveBeenCalledWith('/api/v1/admin/notification-channels/{id}', {
        params: { path: { id: 2 } },
      }),
    )
  })
})
