import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createMemoryRouter, RouterProvider } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { AppShell } from '@/components/AppShell'
import { ok, renderWithProviders, UNLOCKED_MEMBER, LOCKED_MEMBER } from '@/test/render'

import { LegacyRedirect } from './LegacyRedirect'
import { setShortcutsEnabled } from './prefs'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

/** Pretend the window is `width` px wide for the shell's media queries. */
function setWidth(width: number) {
  window.matchMedia = ((query: string) => {
    const min = Number(/min-width:\s*(\d+)px/.exec(query)?.[1] ?? 0)
    return {
      matches: width >= min,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    }
  }) as unknown as typeof window.matchMedia
}

function serve(me: unknown, extra: Record<string, unknown> = {}) {
  api.GET.mockImplementation((path: string) => {
    if (path === '/api/v1/auth/me') return Promise.resolve(ok(me))
    if (path in extra) return Promise.resolve(ok(extra[path]))
    if (path.includes('notifications'))
      return Promise.resolve(ok({ unread: 0, nextBefore: null, items: [] }))
    return Promise.resolve(ok([]))
  })
}

const MEMBER = { kind: 'member', username: 'mia', displayName: 'Mia' }
const ADMIN = { kind: 'admin', username: 'lena', displayName: 'Lena', totpEnabled: true }

const original = window.matchMedia

beforeEach(() => {
  api.GET.mockReset()
  api.PUT.mockReset()
  serve(MEMBER)
})

afterEach(() => {
  window.matchMedia = original
  localStorage.clear()
  setShortcutsEnabled(true)
  document.title = ''
})

const toggle = () => screen.getByRole('button', { name: /(Collapse|Expand) sidebar/ })

describe('sidebar (spec 0008 W1, Q10)', () => {
  it('is expanded at 1440 px, collapses to a rail and remembers it', async () => {
    setWidth(1440)
    const first = renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    expect(toggle().getAttribute('aria-expanded')).toBe('true')
    expect(toggle().getAttribute('aria-controls')).toBe('sidebar')
    expect(document.getElementById('sidebar')).not.toBeNull()
    fireEvent.click(toggle())
    expect(toggle().getAttribute('aria-expanded')).toBe('false')
    expect(toggle().getAttribute('aria-label')).toBe('Expand sidebar')
    expect(localStorage.getItem('starr.sidebar')).toBe('rail')
    first.unmount()

    renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    expect(toggle().getAttribute('aria-expanded')).toBe('false')
  })

  it('keeps every link named in the rail', async () => {
    setWidth(1440)
    localStorage.setItem('starr.sidebar', 'rail')
    renderWithProviders(<AppShell variant="member" />)
    const [nav] = await screen.findAllByRole('navigation', { name: 'Main' })
    expect(within(nav as HTMLElement).getByRole('link', { name: 'Discover' })).toBeTruthy()
    expect(within(nav as HTMLElement).getByRole('link', { name: 'My services' })).toBeTruthy()
  })

  it('starts as a rail at 1024 px and expands over the content for the moment', async () => {
    setWidth(1100)
    renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    expect(toggle().getAttribute('aria-expanded')).toBe('false')
    fireEvent.click(toggle())
    expect(toggle().getAttribute('aria-expanded')).toBe('true')
    expect(document.getElementById('sidebar')?.className).toMatch(/\bfixed\b/)
    // Not remembered: the overlay is for the moment only.
    expect(localStorage.getItem('starr.sidebar')).toBeNull()
    fireEvent.keyDown(document, { key: 'Escape' })
    expect(toggle().getAttribute('aria-expanded')).toBe('false')
    expect(document.activeElement).toBe(toggle())
  })

  it('closes the 1024 px overlay on a click outside, not on a click in its menus', async () => {
    setWidth(1100)
    renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    fireEvent.click(toggle())
    const portal = document.createElement('div')
    portal.setAttribute('data-radix-popper-content-wrapper', '')
    document.body.append(portal)
    fireEvent.pointerDown(portal)
    expect(toggle().getAttribute('aria-expanded')).toBe('true')
    portal.remove()
    fireEvent.pointerDown(document.getElementById('main') as HTMLElement)
    expect(toggle().getAttribute('aria-expanded')).toBe('false')
  })

  it('names the private area in the sidebar only while unlocked (spec 0007: absent)', async () => {
    for (const me of [LOCKED_MEMBER, UNLOCKED_MEMBER]) {
      serve(me)
      const { unmount } = renderWithProviders(<AppShell variant="member" />)
      await screen.findAllByRole('button', { name: /Cara/ })
      const [nav] = screen.getAllByRole('navigation', { name: 'Main' })
      const links = within(nav as HTMLElement)
        .getAllByRole('link')
        .map((l) => l.getAttribute('href'))
      if (me === UNLOCKED_MEMBER) {
        expect(links).toEqual(
          expect.arrayContaining(['/private', '/private/requests', '/private/following']),
        )
      } else {
        expect(links.some((h) => h?.startsWith('/private'))).toBe(false)
        expect(document.body.innerHTML).not.toMatch(/\/private|Private/)
      }
      unmount()
    }
  })

  it('toggles with [ unless typing or switched off (WCAG 2.1.4)', async () => {
    setWidth(1440)
    renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    fireEvent.keyDown(document.body, { key: '[' })
    expect(toggle().getAttribute('aria-expanded')).toBe('false')
    fireEvent.keyDown(document.body, { key: '[' })
    expect(toggle().getAttribute('aria-expanded')).toBe('true')

    const [search] = screen.getAllByRole('searchbox')
    fireEvent.keyDown(search as HTMLElement, { key: '[' })
    expect(toggle().getAttribute('aria-expanded')).toBe('true')

    act(() => setShortcutsEnabled(false))
    fireEvent.keyDown(document.body, { key: '[' })
    expect(toggle().getAttribute('aria-expanded')).toBe('true')
    expect(toggle().getAttribute('aria-keyshortcuts')).toBeNull()
  })

  it('/ jumps to the search field', async () => {
    setWidth(1440)
    renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    const field = screen.getByRole('searchbox')
    // jsdom has no layout: pretend the field is rendered.
    Object.defineProperty(field, 'offsetParent', { get: () => document.body })
    fireEvent.keyDown(document.body, { key: '/' })
    expect(document.activeElement).toBe(field)
  })

  it('lists the categories the member may browse, with Discover first', async () => {
    serve(MEMBER, {
      '/api/v1/discover': [
        { category: 'movies', discoverReady: true, kind: 'movie', lists: [], requestable: true },
        { category: 'music', discoverReady: false, kind: 'artist', lists: [], requestable: true },
        { category: 'series', discoverReady: true, kind: 'series', lists: [], requestable: true },
      ],
    })
    renderWithProviders(<AppShell variant="member" />)
    const [nav] = await screen.findAllByRole('navigation', { name: 'Main' })
    await within(nav as HTMLElement).findByRole('link', { name: 'Movies' })
    const names = within(nav as HTMLElement)
      .getAllByRole('link')
      .map((l) => l.textContent)
    expect(names).toEqual(['Discover', 'Movies', 'Series', 'Search', 'Requests', 'My services'])
    expect(
      within(nav as HTMLElement)
        .getByRole('link', { name: 'Series' })
        .getAttribute('href'),
    ).toBe('/discover/series')
  })

  it('gives the admin the grouped navigation with the Inbox count as description (ADM-23)', async () => {
    const attention = {
      total: 2,
      sections: { decide: 1, fix: 1, paused: 0, setup: 0 },
      items: [
        { id: 'approval:1', kind: 'approval', section: 'decide', redacted: false },
        { id: 'failed:2', kind: 'failed', section: 'fix', redacted: false },
      ],
    }
    api.GET.mockImplementation((path: string) => {
      if (path === '/api/v1/auth/me') return Promise.resolve(ok(ADMIN))
      if (path === '/api/v1/admin/attention') return Promise.resolve(ok(attention))
      if (path.includes('notifications'))
        return Promise.resolve(ok({ unread: 0, nextBefore: null, items: [] }))
      return Promise.resolve(ok([]))
    })
    renderWithProviders(<AppShell variant="admin" />, { path: '/admin/inbox' })
    const [nav] = await screen.findAllByRole('navigation', { name: 'Administration' })
    const scope = within(nav as HTMLElement)
    for (const group of ['Requests', 'People', 'Media stack', 'System'])
      expect(scope.getByRole('group', { name: group })).toBeTruthy()
    const inbox = scope.getByRole('link', { name: 'Inbox' })
    expect(inbox.getAttribute('aria-current')).toBe('page')
    await waitFor(() => expect(inbox.getAttribute('aria-describedby')).toBeTruthy())
    const description = document.getElementById(inbox.getAttribute('aria-describedby') as string)
    expect(description?.textContent).toBe('2 items need you')
    // The same number leads the tab title (spec 0008 F9).
    await waitFor(() => expect(document.title).toBe('(2) Inbox · Starr'))
    expect(scope.getByRole('link', { name: 'Downloads' }).getAttribute('href')).toBe(
      '/admin/downloads',
    )
  })
})

describe('mobile tab bar (F-6)', () => {
  it('has Discover, Search, Requests, Profile, and Private only while unlocked', async () => {
    for (const me of [LOCKED_MEMBER, UNLOCKED_MEMBER]) {
      serve(me)
      const { unmount } = renderWithProviders(<AppShell variant="member" />, {
        path: '/discover/movies',
      })
      const navs = await screen.findAllByRole('navigation', { name: 'Main' })
      const bar = navs[navs.length - 1] as HTMLElement
      await screen.findAllByRole('button', { name: /Cara/ })
      const names = within(bar)
        .getAllByRole('link')
        .map((l) => l.textContent)
      const unlocked = me === UNLOCKED_MEMBER
      expect(names).toEqual(
        unlocked
          ? ['Discover', 'Search', 'Requests', 'Profile', 'Private']
          : ['Discover', 'Search', 'Requests', 'Profile'],
      )
      // Discover stays active on the category pages.
      expect(within(bar).getByRole('link', { name: 'Discover' }).getAttribute('aria-current')).toBe(
        'page',
      )
      unmount()
    }
  })
})

describe('member view (MEM-19)', () => {
  it('shows a persistent strip with the way back to admin', async () => {
    serve({ ...MEMBER, switchedFrom: { displayName: 'Lena', totpEnabled: true } })
    renderWithProviders(<AppShell variant="member" />)
    const strip = await screen.findByRole('region', { name: 'Member view' })
    fireEvent.click(within(strip).getByRole('button', { name: 'Back to admin' }))
    expect(await screen.findByRole('dialog', { name: 'Back to admin' })).toBeTruthy()
  })

  it('is absent for ordinary members', async () => {
    renderWithProviders(<AppShell variant="member" />)
    await screen.findAllByRole('button', { name: /Mia/ })
    expect(screen.queryByRole('region', { name: 'Member view' })).toBeNull()
  })
})

describe('document.title (MEM-18)', () => {
  it('names the page, and stays neutral in the private area', async () => {
    serve(UNLOCKED_MEMBER)
    renderWithProviders(<AppShell variant="member" />, { path: '/requests' })
    await waitFor(() => expect(document.title).toBe('Requests · Starr'))
  })

  it('says only "Starr" inside the private area', async () => {
    serve(UNLOCKED_MEMBER)
    renderWithProviders(<AppShell variant="member" />, { path: '/private/scenes/abc' })
    await screen.findAllByRole('button', { name: /Cara/ })
    await waitFor(() => expect(document.title).toBe('Starr'))
  })
})

describe('old admin routes (Q22)', () => {
  it('replace the old entry on a first load too (bookmark, email link)', async () => {
    const router = createMemoryRouter(
      [
        { path: '/start', element: <p>start</p> },
        { path: '/admin/needs-you', element: <LegacyRedirect /> },
        { path: '/admin/inbox', element: <p>inbox</p> },
      ],
      { initialEntries: ['/start', '/admin/needs-you?open=3'], initialIndex: 1 },
    )
    render(
      <QueryClientProvider client={new QueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    )
    expect(await screen.findByText('inbox')).toBeTruthy()
    expect(router.state.location.search).toBe('?open=3')
    await act(() => router.navigate(-1))
    await waitFor(() => expect(router.state.location.pathname).toBe('/start'))
  })

  it('redirect with replace and keep the query', async () => {
    const router = createMemoryRouter(
      [
        { path: '/start', element: <p>start</p> },
        { path: '/admin/runs', element: <LegacyRedirect /> },
        { path: '/admin/system/runs', element: <p>runs</p> },
      ],
      { initialEntries: ['/start'] },
    )
    render(
      <QueryClientProvider client={new QueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    )
    expect(await screen.findByText('start')).toBeTruthy()
    await act(() => router.navigate('/admin/runs?run=4'))
    expect(await screen.findByText('runs')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/admin/system/runs')
    expect(router.state.location.search).toBe('?run=4')
    // Replaced, not pushed: Back leaves the old URL behind.
    await act(() => router.navigate(-1))
    await waitFor(() => expect(router.state.location.pathname).toBe('/start'))
  })
})
