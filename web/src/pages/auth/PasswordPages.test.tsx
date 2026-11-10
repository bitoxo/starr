import { fireEvent, screen, waitFor } from '@testing-library/react'
import { Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { fail, ok, renderWithProviders } from '@/test/render'

import { SetPasswordPage } from './PasswordPages'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const MEMBER = { kind: 'member', username: 'mia', displayName: 'Mia' }

function renderPage() {
  return renderWithProviders(
    <Routes>
      <Route path="/set-password/:token" element={<SetPasswordPage />} />
      <Route path="/" element={<p>member home</p>} />
      <Route path="/login" element={<p>login page</p>} />
    </Routes>,
    { path: '/set-password/tok-1' },
  )
}

function type(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } })
}

describe('set password (AUTH-03)', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
  })

  it('checks the link on load before asking for a password', async () => {
    api.GET.mockResolvedValueOnce(ok({ expiresAt: '2026-11-14T10:00:00Z' }))
    renderPage()
    expect(screen.getByRole('status').textContent).toBe('Checking your link…')
    expect(await screen.findByLabelText('Confirm password')).toBeTruthy()
    expect(api.GET).toHaveBeenCalledWith('/api/v1/password-tokens/{token}', {
      params: { path: { token: 'tok-1' } },
    })
    expect(screen.getByText(/This link expires/)).toBeTruthy()
    expect(screen.getByLabelText(/^Password/)).toBeTruthy()
  })

  it.each([
    [
      'password_token.not_found',
      404,
      "This link isn't valid. It may have been replaced by a newer one.",
    ],
    ['password_token.used', 409, 'This link was already used. Ask for a new one.'],
    ['password_token.expired', 409, 'This link expired. Ask for a new one.'],
    ['password_token.account_inactive', 409, 'This account is suspended. Ask your admin.'],
  ])('says why a link (%s) cannot be used and what to do next', async (code, status, message) => {
    api.GET.mockResolvedValueOnce(fail(code, status))
    renderPage()
    expect(await screen.findByRole('heading', { name: "This link can't be used" })).toBeTruthy()
    expect(screen.getByText(message)).toBeTruthy()
    expect(
      screen.getByText('Ask your admin for a new password link. Only the newest link works.'),
    ).toBeTruthy()
    expect(screen.queryByLabelText(/^Password/)).toBeNull()
    expect(screen.getByRole('link', { name: 'Go to sign in' }).getAttribute('href')).toBe('/login')
    expect(api.POST).not.toHaveBeenCalled()
  })

  it('offers a retry when the check itself failed, without calling the link invalid', async () => {
    api.GET.mockResolvedValueOnce(fail('auth.too_many_attempts', 429))
    renderPage()
    expect(await screen.findByRole('heading', { name: "We couldn't check this link" })).toBeTruthy()
    expect(screen.queryByText(/Only the newest link works/)).toBeNull()
    expect(screen.getByRole('link', { name: 'Go to sign in' })).toBeTruthy()
    api.GET.mockResolvedValueOnce(ok({ expiresAt: '2026-11-14T10:00:00Z' }))
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
    expect(await screen.findByLabelText('Confirm password')).toBeTruthy()
  })

  it('sets the password and signs in with a valid link', async () => {
    api.GET.mockResolvedValueOnce(ok({ expiresAt: '2026-11-14T10:00:00Z' }))
    renderPage()
    await screen.findByLabelText('Confirm password')
    api.POST.mockResolvedValueOnce(ok(MEMBER))
    type('Password', 'a long new password')
    type('Confirm password', 'a long new password')
    fireEvent.click(screen.getByRole('button', { name: 'Save password' }))
    expect(await screen.findByText('member home')).toBeTruthy()
    // The used link is not checked again.
    expect(api.GET).toHaveBeenCalledTimes(1)
  })

  it('moves focus to the error when saving fails', async () => {
    api.GET.mockResolvedValueOnce(ok({ expiresAt: '2026-11-14T10:00:00Z' }))
    renderPage()
    await screen.findByLabelText('Confirm password')
    api.POST.mockResolvedValueOnce(fail('password_token.used', 409))
    type('Password', 'a long new password')
    type('Confirm password', 'a long new password')
    fireEvent.click(screen.getByRole('button', { name: 'Save password' }))
    const alert = await screen.findByRole('alert')
    await waitFor(() => expect(document.activeElement).toBe(alert))
  })

  it('allows pasting into both password fields (WCAG 3.3.8)', async () => {
    api.GET.mockResolvedValueOnce(ok({ expiresAt: '2026-11-14T10:00:00Z' }))
    renderPage()
    await screen.findByLabelText('Confirm password')
    expect(fireEvent.paste(screen.getByLabelText(/^Password/))).toBe(true)
    expect(fireEvent.paste(screen.getByLabelText('Confirm password'))).toBe(true)
  })
})
