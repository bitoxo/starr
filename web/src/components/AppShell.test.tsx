import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { MemoryRouter, Route, Routes } from 'react-router'

import { ok, renderWithProviders } from '@/test/render'

import { AppShell } from './AppShell'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

describe('AppShell', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.GET.mockImplementation((path: string) => {
      if (path === '/api/v1/auth/me')
        return Promise.resolve(ok({ kind: 'member', displayName: 'Mia' }))
      if (path.includes('notifications'))
        return Promise.resolve(ok({ unread: 0, nextBefore: null, items: [] }))
      return Promise.resolve(ok([]))
    })
  })

  async function openAccountMenu() {
    const [trigger] = await screen.findAllByRole('button', { name: /Mia/ })
    fireEvent.keyDown(trigger as HTMLElement, { key: 'Enter' })
    return screen.findByRole('menu')
  }

  it('loads the detail overlay for a deep link and closes it to the page heading (slice 12)', async () => {
    renderWithProviders(
      <Routes>
        <Route element={<AppShell variant="member" />}>
          <Route path="/" element={<h1>Discover</h1>} />
        </Route>
      </Routes>,
      { path: '/?title=movie-27205' },
    )
    const dialog = await screen.findByRole('dialog', {}, { timeout: 3000 })
    fireEvent.keyDown(dialog, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'Discover' }))
  })

  it('names no private category for accounts that may not unlock it', async () => {
    renderWithProviders(<AppShell variant="member" />)
    const menu = await openAccountMenu()
    expect(within(menu).queryByText(/private category/i)).toBeNull()
    expect(document.body.textContent).not.toMatch(/NSFW|adult/i)
  })

  it('unlocks the private category for the session from the account menu', async () => {
    api.GET.mockImplementation((path: string) => {
      if (path === '/api/v1/auth/me')
        return Promise.resolve(
          ok({
            kind: 'member',
            displayName: 'Mia',
            privateCategory: { unlocked: false, unlockedUntil: null },
          }),
        )
      if (path.includes('notifications'))
        return Promise.resolve(ok({ unread: 0, nextBefore: null, items: [] }))
      return Promise.resolve(ok([]))
    })
    api.PUT.mockResolvedValue(ok({ unlocked: true, unlockedUntil: '2026-11-03T12:30:00Z' }))
    renderWithProviders(<AppShell variant="member" />)
    const menu = await openAccountMenu()
    fireEvent.click(within(menu).getByRole('menuitem', { name: /^Show Private/ }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/auth/private', { body: { unlocked: true } }),
    )
  })

  it('lists the private area in the menu only while the session has unlocked it', async () => {
    for (const unlocked of [false, true]) {
      api.GET.mockImplementation((path: string) => {
        if (path === '/api/v1/auth/me')
          return Promise.resolve(
            ok({
              kind: 'member',
              displayName: 'Mia',
              privateCategory: {
                unlocked,
                unlockedUntil: unlocked ? '2026-11-03T12:30:00Z' : null,
              },
            }),
          )
        if (path.includes('notifications'))
          return Promise.resolve(ok({ unread: 0, nextBefore: null, items: [] }))
        return Promise.resolve(ok([]))
      })
      const { unmount } = renderWithProviders(<AppShell variant="member" />)
      const [nav] = await screen.findAllByRole('navigation', { name: 'Main' })
      await screen.findAllByRole('button', { name: /Mia/ })
      const link = within(nav as HTMLElement).queryByRole('link', { name: 'Private' })
      if (unlocked) expect(link?.getAttribute('href')).toBe('/private')
      else expect(link).toBeNull()
      unmount()
    }
  })

  function serveGate(unlocked: boolean) {
    api.GET.mockImplementation((path: string) => {
      if (path === '/api/v1/auth/me')
        return Promise.resolve(
          ok({
            kind: 'member',
            username: 'mia',
            displayName: 'Mia',
            privateCategory: {
              unlocked,
              unlockedUntil: unlocked ? '2026-11-03T21:14:00Z' : null,
            },
          }),
        )
      if (path.includes('notifications'))
        return Promise.resolve(ok({ unread: 0, nextBefore: null, items: [] }))
      return Promise.resolve(ok([]))
    })
  }

  it('shows "Private open until" with Close only while unlocked (PRIV-04)', async () => {
    serveGate(false)
    const locked = renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    expect(locked.container.querySelector('[data-private-gate]')).toBeNull()
    expect(screen.queryByRole('button', { name: 'Close private area' })).toBeNull()
    locked.unmount()

    serveGate(true)
    api.PUT.mockImplementation(() => {
      serveGate(false)
      return Promise.resolve(ok({ unlocked: false, unlockedUntil: null }))
    })
    const { container } = renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    // Header bar (desktop) and sticky strip (mobile).
    await waitFor(() => expect(container.querySelectorAll('[data-private-gate]')).toHaveLength(2))
    const bar = container.querySelector('[data-private-gate]')
    expect(bar?.textContent).toMatch(/Private open until/)
    expect(bar?.querySelector('time')?.getAttribute('dateTime')).toBe('2026-11-03T21:14:00Z')
    const [close] = screen.getAllByRole('button', { name: 'Close private area' })
    fireEvent.click(close as HTMLElement)
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/auth/private', { body: { unlocked: false } }),
    )
    // Gone at once, before anything is fetched again.
    await waitFor(() => expect(container.querySelector('[data-private-gate]')).toBeNull())
    expect(screen.getByRole('status').textContent).toBe('Private closed.')
    // The Close button went with the indicator: focus moves to the page (slice 12).
    await waitFor(() => expect(document.activeElement?.id).toBe('main'))
  })

  it('after the client closed the area, says so neutrally and offers to show it again', async () => {
    serveGate(false)
    api.PUT.mockResolvedValue(ok({ unlocked: true, unlockedUntil: '2026-11-03T21:44:00Z' }))
    renderWithProviders(
      <MemoryRouter initialEntries={[{ pathname: '/', state: { privateGateClosed: 'idle' } }]}>
        <AppShell variant="member" />
      </MemoryRouter>,
      { router: false },
    )
    expect(
      await screen.findByText('The private area closed after 30 minutes without activity.'),
    ).toBeTruthy()
    expect(document.body.textContent).not.toMatch(/NSFW|adult|scene|performer|studio/i)
    fireEvent.click(await screen.findByRole('button', { name: 'Show again' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/auth/private', { body: { unlocked: true } }),
    )
    await waitFor(() => expect(screen.queryByText(/closed after 30 minutes/)).toBeNull())
  })

  it('gives members only what they can use: Discover, Search, Requests, My services', async () => {
    renderWithProviders(<AppShell variant="member" />)
    const [desktop] = await screen.findAllByRole('navigation', { name: 'Main' })
    const links = within(desktop as HTMLElement)
      .getAllByRole('link')
      .map((l) => l.textContent)
    expect(links).toEqual(['Discover', 'Search', 'Requests', 'My services'])
    expect(screen.queryByText('Library')).toBeNull()
    expect(screen.queryByText('Movies')).toBeNull()
    expect(screen.queryByText('soon')).toBeNull()
  })

  it.each(['member', 'admin'] as const)(
    'lets the %s sidebar ground grow with the page while its body stays in view',
    (variant) => {
      const { container } = renderWithProviders(<AppShell variant={variant} />)
      const aside = container.querySelector('aside') as HTMLElement
      // The aside itself has no fixed height: it stretches with the page.
      expect(aside.className).not.toMatch(/\bh-dvh\b|\bsticky\b/)
      expect(aside.className).toMatch(/\bbg-night-1\b/)
      const body = aside.firstElementChild as HTMLElement
      expect(body.className).toMatch(/\bsticky\b/)
      expect(body.className).toMatch(/\bh-dvh\b/)
    },
  )
})
