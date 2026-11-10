import { fireEvent, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { fail, ok, renderWithProviders } from '@/test/render'

import { AddAdminDialog } from './AdminsPage'
import {
  ConfirmTotpForm,
  DisableTotpDialog,
  PasswordCard,
  RecoveryCodesPanel,
  RegenerateCodesDialog,
  SessionsCard,
} from './SecurityPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const CODES = Array.from({ length: 10 }, (_, i) => `ABCD-EFGH-JK${i + 20}`)

function type(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } })
}

describe('TOTP enrollment', () => {
  beforeEach(() => api.POST.mockReset())

  it('turns TOTP on with a valid code and hands over the recovery codes', async () => {
    const onConfirmed = vi.fn()
    api.POST.mockResolvedValueOnce(ok({ codes: CODES }))
    renderWithProviders(<ConfirmTotpForm onConfirmed={onConfirmed} />)
    type('Code from the app', '123456')
    fireEvent.click(screen.getByRole('button', { name: 'Turn on' }))
    await waitFor(() => expect(onConfirmed).toHaveBeenCalledWith(CODES))
    expect(api.POST).toHaveBeenCalledWith('/api/v1/auth/totp/confirm', {
      body: { code: '123456' },
    })
  })

  it('shows a wrong code and stays on the form', async () => {
    const onConfirmed = vi.fn()
    api.POST.mockResolvedValueOnce(fail('auth.invalid_code', 422))
    renderWithProviders(<ConfirmTotpForm onConfirmed={onConfirmed} />)
    type('Code from the app', '000000')
    fireEvent.click(screen.getByRole('button', { name: 'Turn on' }))
    expect((await screen.findByRole('alert')).textContent).toContain("That code didn't work")
    expect(onConfirmed).not.toHaveBeenCalled()
  })

  it('lists every recovery code once, with copy and download', () => {
    const onDone = vi.fn()
    renderWithProviders(<RecoveryCodesPanel codes={CODES} onDone={onDone} />)
    const list = screen.getByRole('list', { name: 'Save your recovery codes' })
    expect(list.querySelectorAll('li')).toHaveLength(10)
    expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Download' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: "I've saved them" }))
    expect(onDone).toHaveBeenCalled()
  })

  it('needs password and code to turn TOTP off', async () => {
    api.POST.mockResolvedValueOnce(ok(undefined, 204))
    const onOpenChange = vi.fn()
    renderWithProviders(<DisableTotpDialog open onOpenChange={onOpenChange} />)
    fireEvent.click(screen.getByRole('button', { name: 'Turn off' }))
    expect(await screen.findByText('This field is required.')).toBeTruthy()
    expect(api.POST).not.toHaveBeenCalled()

    type('Password', 'correct horse')
    type('Code', 'abcd-efgh-jk23')
    fireEvent.click(screen.getByRole('button', { name: 'Turn off' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/auth/totp/disable', {
        body: { password: 'correct horse', code: 'abcd-efgh-jk23' },
      }),
    )
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false))
  })
})

describe('new recovery codes', () => {
  beforeEach(() => api.POST.mockReset())

  it('need password and a TOTP code, not a recovery code', async () => {
    const onRegenerated = vi.fn()
    renderWithProviders(
      <RegenerateCodesDialog open onOpenChange={() => {}} onRegenerated={onRegenerated} />,
    )
    type('Password', 'correct horse')
    type('Code from the app', 'abcd-efgh-jk23')
    fireEvent.click(screen.getByRole('button', { name: 'Replace codes' }))
    expect(await screen.findByText('Enter the 6-digit code from your app.')).toBeTruthy()
    expect(api.POST).not.toHaveBeenCalled()

    api.POST.mockResolvedValueOnce(ok({ codes: CODES }))
    type('Code from the app', '123456')
    fireEvent.click(screen.getByRole('button', { name: 'Replace codes' }))
    await waitFor(() => expect(onRegenerated).toHaveBeenCalledWith(CODES))
    expect(api.POST).toHaveBeenCalledWith('/api/v1/auth/totp/recovery-codes', {
      body: { password: 'correct horse', code: '123456' },
    })
  })
})

describe('own password and sessions (ADM-28)', () => {
  beforeEach(() => {
    api.POST.mockReset()
    api.GET.mockReset()
    api.DELETE.mockReset()
  })

  it('changes the password with the current one', async () => {
    api.POST.mockResolvedValueOnce(ok(undefined, 204))
    renderWithProviders(<PasswordCard />)
    type('Current password', 'old secret password')
    type('New password', 'brand new password')
    type('Confirm password', 'brand new password')
    fireEvent.click(screen.getByRole('button', { name: 'Change password' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/me/password', {
        body: { currentPassword: 'old secret password', newPassword: 'brand new password' },
      }),
    )
  })

  it('says when the current password is wrong', async () => {
    api.POST.mockResolvedValueOnce(fail('auth.current_password_invalid', 422))
    renderWithProviders(<PasswordCard />)
    type('Current password', 'nope nope nope')
    type('New password', 'brand new password')
    type('Confirm password', 'brand new password')
    fireEvent.click(screen.getByRole('button', { name: 'Change password' }))
    expect(await screen.findByRole('alert')).toBeTruthy()
  })

  it('lists the sessions and signs out another device, never this one', async () => {
    api.GET.mockResolvedValue(
      ok([
        {
          id: 1,
          current: true,
          createdAt: '2026-11-07T08:00:00Z',
          lastSeenAt: '2026-11-07T09:00:00Z',
          ip: '10.0.0.2',
          userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Firefox/140.0',
        },
        {
          id: 2,
          current: false,
          createdAt: '2026-11-01T08:00:00Z',
          lastSeenAt: '2026-11-02T09:00:00Z',
          ip: '10.0.0.9',
          userAgent: null,
        },
      ]),
    )
    api.DELETE.mockResolvedValueOnce(ok(undefined, 204))
    renderWithProviders(<SessionsCard />)
    expect(await screen.findByText(/This device/)).toBeTruthy()
    const buttons = screen.getAllByRole('button', { name: /^Sign out/ })
    expect(buttons).toHaveLength(1)
    fireEvent.click(screen.getByRole('button', { name: 'Sign out Unknown device' }))
    await waitFor(() =>
      expect(api.DELETE).toHaveBeenCalledWith('/api/v1/admin/me/sessions/{id}', {
        params: { path: { id: 2 } },
      }),
    )
  })

  it('offers a retry when the sessions cannot be loaded', async () => {
    api.GET.mockResolvedValue(fail('internal', 500))
    renderWithProviders(<SessionsCard />)
    expect(await screen.findByRole('button', { name: 'Try again' })).toBeTruthy()
  })
})

describe('admin accounts', () => {
  beforeEach(() => api.POST.mockReset())

  it('creates an admin and shows the one-time link', async () => {
    api.POST.mockResolvedValueOnce(
      ok(
        {
          admin: { id: 2, username: 'ada', displayName: 'Ada' },
          setPasswordLink: {
            url: 'https://starr.test/set-password/tok',
            expiresAt: '2026-11-08T12:00:00Z',
          },
        },
        201,
      ),
    )
    renderWithProviders(<AddAdminDialog open onOpenChange={() => {}} />)
    type('Username', 'Ada')
    fireEvent.click(screen.getByRole('button', { name: 'Create admin' }))
    expect(await screen.findByRole('heading', { name: 'Admin created' })).toBeTruthy()
    expect(api.POST).toHaveBeenCalledWith('/api/v1/admins', {
      body: { username: 'ada', displayName: undefined },
    })
    expect(screen.getByDisplayValue('https://starr.test/set-password/tok')).toBeTruthy()
  })

  it('rejects an invalid username before sending', async () => {
    renderWithProviders(<AddAdminDialog open onOpenChange={() => {}} />)
    type('Username', 'ab')
    fireEvent.click(screen.getByRole('button', { name: 'Create admin' }))
    expect(await screen.findByText('Use 3 to 32 characters.')).toBeTruthy()
    expect(api.POST).not.toHaveBeenCalled()
  })
})
