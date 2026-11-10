import { fireEvent, screen } from '@testing-library/react'

import { Route, Routes } from 'react-router'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { api } from '@/api/client'
import { setShortcutsEnabled } from '@/components/shell/prefs'
import { ok, renderWithProviders } from '@/test/render'

import ProfilePage, { ProfileNotifications, ProfilePreferences } from './ProfilePage'

vi.mock('@/api/client', () => ({ api: { GET: vi.fn(() => new Promise(() => {})) } }))

afterEach(() => {
  localStorage.clear()
  setShortcutsEnabled(true)
})

function renderAt(path: string) {
  return renderWithProviders(
    <Routes>
      <Route path="/profile" element={<ProfilePage />}>
        <Route path="preferences" element={<ProfilePreferences />} />
        <Route path="notifications" element={<ProfileNotifications />} />
      </Route>
    </Routes>,
    { path },
  )
}

describe('Profile', () => {
  it('has its sections as routes, and My services for small screens (F-6)', () => {
    renderAt('/profile/preferences')
    const nav = screen.getByRole('navigation', { name: 'Profile sections' })
    const links = Array.from(nav.querySelectorAll('a')).map((a) => a.getAttribute('href'))
    expect(links).toEqual([
      '/profile',
      '/profile/notifications',
      '/profile/sessions',
      '/profile/preferences',
      '/services',
    ])
    expect(screen.getByRole('link', { name: 'Preferences' }).getAttribute('aria-current')).toBe(
      'page',
    )
  })

  it('switches single-key shortcuts off for this browser (WCAG 2.1.4)', () => {
    renderAt('/profile/preferences')
    const toggle = screen.getByRole('switch', { name: 'Single-key shortcuts' })
    expect(toggle.getAttribute('aria-checked')).toBe('true')
    fireEvent.click(toggle)
    expect(toggle.getAttribute('aria-checked')).toBe('false')
    expect(localStorage.getItem('starr.shortcuts')).toBe('off')
  })

  it('adds the email choices only once the list above is there (no layout shift, slice 12)', () => {
    renderAt('/profile/notifications')
    expect(screen.getByRole('heading', { name: 'All notifications' })).toBeTruthy()
    expect(screen.queryByRole('heading', { name: 'By email' })).toBeNull()
  })

  it('shows the email choices once the list is there', async () => {
    vi.mocked(api.GET).mockImplementation(((path: string) =>
      path === '/api/v1/notifications'
        ? Promise.resolve(ok({ items: [], unread: 0, nextBefore: null }))
        : new Promise(() => {})) as unknown as typeof api.GET)
    renderAt('/profile/notifications')
    expect(await screen.findByRole('heading', { name: 'By email' })).toBeTruthy()
  })
})
