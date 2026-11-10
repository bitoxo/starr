import { fireEvent, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { fail, ok, renderWithProviders } from '@/test/render'

import ForgotPasswordPage from './ForgotPasswordPage'
import SetupPage from './SetupPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

describe('forgot password (AUTH-01)', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.GET.mockResolvedValue(ok({ posters: [] }))
  })

  it('explains the reset link from the admin and leads back to sign in', () => {
    renderWithProviders(<ForgotPasswordPage />)
    expect(screen.getByRole('heading', { level: 1, name: 'Forgot your password?' })).toBeTruthy()
    expect(screen.getByText(/Ask your admin for a reset link/)).toBeTruthy()
    expect(screen.getByText('Ask your admin for a new password link.')).toBeTruthy()
    expect(screen.getByText('starr admin recover <username>')).toBeTruthy()
    const back = screen.getByRole('link', { name: 'Back to sign in' })
    expect(back.getAttribute('href')).toBe('/login')
  })
})

describe('first-run setup', () => {
  beforeEach(() => api.POST.mockReset())

  it('previews the setup checklist that waits in the Inbox', () => {
    renderWithProviders(<SetupPage />)
    const next = screen.getByRole('region', { name: 'After this' })
    expect(next.textContent).toContain('Connect services')
    expect(next.textContent).toContain('Switch on a category')
    expect(next.textContent).toContain('Turn on two-step verification for your account')
    expect(next.textContent).toContain('Invite members')
  })

  it('moves focus to the error when setup fails', async () => {
    api.POST.mockResolvedValueOnce(fail('setup.invalid_token', 403))
    renderWithProviders(<SetupPage />)
    fireEvent.change(screen.getByLabelText('Setup token'), { target: { value: 'nope' } })
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'root' } })
    fireEvent.change(screen.getByLabelText(/^Password/), {
      target: { value: 'a long admin password' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Create admin account' }))
    const alert = await screen.findByRole('alert')
    expect(alert.textContent).toContain("That setup token isn't valid.")
    await waitFor(() => expect(document.activeElement).toBe(alert))
  })
})
