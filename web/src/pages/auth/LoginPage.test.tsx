import { fireEvent, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { fail, ok, renderWithProviders } from '@/test/render'

import LoginPage from './LoginPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const ME = { kind: 'admin', username: 'root', displayName: 'Root', totpEnabled: true }

function type(label: RegExp | string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } })
}

async function passwordStep() {
  type('Username', 'root')
  type('Password', 'correct horse')
  fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))
  return screen.findByRole('heading', { name: 'Two-step verification' })
}

describe('login', () => {
  beforeEach(() => {
    api.POST.mockReset()
    api.GET.mockReset()
    api.GET.mockResolvedValue(ok({ posters: [] }))
  })

  it('asks for a code after the password when TOTP is on', async () => {
    api.POST.mockResolvedValueOnce(ok({ pendingToken: 'pending-1', expiresAt: '' }, 202))
    renderWithProviders(<LoginPage />)
    await passwordStep()

    api.POST.mockResolvedValueOnce(ok(ME))
    type('Code', '123 456')
    fireEvent.click(screen.getByRole('button', { name: 'Verify' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenLastCalledWith('/api/v1/auth/login/totp', {
        body: { pendingToken: 'pending-1', code: '123 456' },
      }),
    )
  })

  it('accepts a recovery code and rejects other input before sending', async () => {
    api.POST.mockResolvedValueOnce(ok({ pendingToken: 'pending-1', expiresAt: '' }, 202))
    renderWithProviders(<LoginPage />)
    await passwordStep()

    type('Code', '12345')
    fireEvent.click(screen.getByRole('button', { name: 'Verify' }))
    expect(await screen.findByText('Enter the 6-digit code or a recovery code.')).toBeTruthy()
    expect(api.POST).toHaveBeenCalledTimes(1)

    api.POST.mockResolvedValueOnce(ok(ME))
    type('Code', 'abcd-efgh-jk23')
    fireEvent.click(screen.getByRole('button', { name: 'Verify' }))
    await waitFor(() => expect(api.POST).toHaveBeenCalledTimes(2))
  })

  it('shows a wrong code and goes back to the password when the login expired', async () => {
    api.POST.mockResolvedValueOnce(ok({ pendingToken: 'pending-1', expiresAt: '' }, 202))
    renderWithProviders(<LoginPage />)
    await passwordStep()

    api.POST.mockResolvedValueOnce(fail('auth.invalid_code', 401))
    type('Code', '000000')
    fireEvent.click(screen.getByRole('button', { name: 'Verify' }))
    expect(await screen.findByRole('alert')).toHaveProperty(
      'textContent',
      "That code didn't work. Codes change every 30 seconds and work once.",
    )

    api.POST.mockResolvedValueOnce(fail('auth.login_expired', 401))
    type('Code', '111111')
    fireEvent.click(screen.getByRole('button', { name: 'Verify' }))
    expect(await screen.findByRole('button', { name: 'Sign in' })).toBeTruthy()
    expect(screen.getByRole('alert').textContent).toBe(
      'The sign-in took too long. Enter your password again.',
    )
  })

  it('signs in directly without TOTP', async () => {
    api.POST.mockResolvedValueOnce(ok({ ...ME, totpEnabled: false }))
    renderWithProviders(<LoginPage />)
    type('Username', 'root')
    type('Password', 'correct horse')
    fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))
    await waitFor(() => expect(api.POST).toHaveBeenCalledTimes(1))
    expect(screen.queryByRole('heading', { name: 'Two-step verification' })).toBeNull()
  })

  it('moves focus to the error after a failed sign-in (AUTH-04)', async () => {
    api.POST.mockResolvedValueOnce(fail('auth.invalid_credentials', 401))
    renderWithProviders(<LoginPage />)
    type('Username', 'root')
    type('Password', 'wrong')
    fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))
    const alert = await screen.findByRole('alert')
    expect(alert.textContent).toBe('Username or password is wrong.')
    await waitFor(() => expect(document.activeElement).toBe(alert))
    expect(document.activeElement).not.toBe(document.body)

    // A second failure focuses the new notice again.
    api.POST.mockResolvedValueOnce(fail('auth.too_many_attempts', 429))
    fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))
    await waitFor(() =>
      expect(document.activeElement?.textContent).toBe(
        'Too many failed attempts. Wait 15 minutes and try again.',
      ),
    )
  })

  it('moves focus to the error after a wrong code', async () => {
    api.POST.mockResolvedValueOnce(ok({ pendingToken: 'pending-1', expiresAt: '' }, 202))
    renderWithProviders(<LoginPage />)
    await passwordStep()
    api.POST.mockResolvedValueOnce(fail('auth.invalid_code', 401))
    type('Code', '000000')
    fireEvent.click(screen.getByRole('button', { name: 'Verify' }))
    const alert = await screen.findByRole('alert')
    await waitFor(() => expect(document.activeElement).toBe(alert))
  })

  it('allows pasting into the password and code fields (WCAG 3.3.8)', async () => {
    api.POST.mockResolvedValueOnce(ok({ pendingToken: 'pending-1', expiresAt: '' }, 202))
    renderWithProviders(<LoginPage />)
    const passwordField = screen.getByLabelText('Password')
    expect(passwordField.getAttribute('autocomplete')).toBe('current-password')
    // `fireEvent` returns false when a handler called preventDefault.
    expect(fireEvent.paste(passwordField)).toBe(true)
    await passwordStep()
    const code = screen.getByLabelText(/^Code/)
    expect(code.getAttribute('autocomplete')).toBe('one-time-code')
    expect(fireEvent.paste(code)).toBe(true)
  })

  it('offers the way to a reset link (AUTH-01)', () => {
    renderWithProviders(<LoginPage />)
    const link = screen.getByRole('link', { name: 'Forgot your password?' })
    expect(link.getAttribute('href')).toBe('/forgot-password')
  })

  it('shows poster art only from the login posters, decorative and never private', async () => {
    const posters = ['a1.jpg', 'b2.jpg', 'c3.jpg', 'd4.jpg', 'e5.jpg']
    api.GET.mockResolvedValue(ok({ posters }))
    const { container } = renderWithProviders(<LoginPage />)
    await waitFor(() => expect(container.querySelector('[data-login-art]')).not.toBeNull())
    const art = container.querySelector('[data-login-art]')
    expect(art?.getAttribute('aria-hidden')).toBe('true')
    const images = Array.from(container.querySelectorAll('img'))
    expect(images.length).toBeGreaterThan(0)
    for (const img of images) {
      expect(img.getAttribute('src')).toMatch(/^\/api\/v1\/login-art\/[a-e]\d\.jpg$/)
      expect(img.getAttribute('alt')).toBe('')
    }
    // The page asks for nothing else: no private area, no image proxy, no stashdb.
    for (const [path] of api.GET.mock.calls) expect(path).toBe('/api/v1/login-art')
    expect(container.innerHTML).not.toMatch(/stashdb|private/i)
  })

  it('leaves the art out while too few posters are cached', async () => {
    api.GET.mockResolvedValue(ok({ posters: ['a1.jpg'] }))
    const { container } = renderWithProviders(<LoginPage />)
    await waitFor(() => expect(api.GET).toHaveBeenCalled())
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(container.querySelector('[data-login-art]')).toBeNull()
    expect(container.querySelector('img')).toBeNull()
  })

  it('encodes poster file names in the image URL and drops the art when the list fails', async () => {
    api.GET.mockResolvedValue(ok({ posters: ['a b.jpg', 'c?d.jpg', 'e.jpg', 'f.jpg'] }))
    const { container, unmount } = renderWithProviders(<LoginPage />)
    await waitFor(() => expect(container.querySelector('img')).not.toBeNull())
    const sources = Array.from(container.querySelectorAll('img')).map((i) => i.getAttribute('src'))
    expect(sources).toContain('/api/v1/login-art/a%20b.jpg')
    expect(sources).toContain('/api/v1/login-art/c%3Fd.jpg')
    unmount()

    api.GET.mockResolvedValue(fail('unavailable', 500))
    const failed = renderWithProviders(<LoginPage />)
    await waitFor(() => expect(api.GET).toHaveBeenCalled())
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(failed.container.querySelector('[data-login-art]')).toBeNull()
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeTruthy()
  })
})
