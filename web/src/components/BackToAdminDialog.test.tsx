import { fireEvent, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { fail, ok, renderWithProviders } from '@/test/render'

import { BackToAdminDialog } from './BackToAdminDialog'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const ADMIN = { kind: 'admin', username: 'root', displayName: 'Root', totpEnabled: true }

function type(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } })
}

describe('back to admin', () => {
  beforeEach(() => api.POST.mockReset())

  it('re-authenticates with password and code, then switches', async () => {
    const onOpenChange = vi.fn()
    api.POST.mockResolvedValueOnce(ok({ elevatedUntil: '' })).mockResolvedValueOnce(ok(ADMIN))
    renderWithProviders(
      <BackToAdminDialog open onOpenChange={onOpenChange} adminName="Root" totpEnabled />,
    )
    type('Admin password', 'correct horse')
    type('Code or recovery code', '123456')
    fireEvent.click(screen.getByRole('button', { name: 'Switch back' }))
    await waitFor(() => expect(api.POST).toHaveBeenCalledTimes(2))
    expect(api.POST.mock.calls[0]).toEqual([
      '/api/v1/auth/elevate',
      { body: { password: 'correct horse', code: '123456' } },
    ])
    expect(api.POST.mock.calls[1]?.[0]).toBe('/api/v1/auth/switch')
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false))
  })

  it('requires the code when the admin has TOTP', async () => {
    renderWithProviders(
      <BackToAdminDialog open onOpenChange={() => {}} adminName="Root" totpEnabled />,
    )
    type('Admin password', 'correct horse')
    fireEvent.click(screen.getByRole('button', { name: 'Switch back' }))
    expect(await screen.findByText('Enter the 6-digit code or a recovery code.')).toBeTruthy()
    expect(api.POST).not.toHaveBeenCalled()
  })

  it('asks only for the password without TOTP and shows a wrong one', async () => {
    api.POST.mockResolvedValueOnce(fail('auth.current_password_invalid', 422))
    renderWithProviders(
      <BackToAdminDialog open onOpenChange={() => {}} adminName="Root" totpEnabled={false} />,
    )
    expect(screen.queryByLabelText('Code or recovery code')).toBeNull()
    type('Admin password', 'wrong')
    fireEvent.click(screen.getByRole('button', { name: 'Switch back' }))
    expect((await screen.findByRole('alert')).textContent).toBe('Current password is wrong.')
    expect(api.POST).toHaveBeenCalledTimes(1)
    expect(api.POST).toHaveBeenCalledWith('/api/v1/auth/elevate', {
      body: { password: 'wrong' },
    })
  })
})
