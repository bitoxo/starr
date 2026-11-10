import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { MyRequest } from '@/api/discover'
import type { Notification, NotificationPage } from '@/api/notifications'
import { fail, NO_JOURNEY, ok, renderWithProviders } from '@/test/render'

import { NotificationBell, NotificationHistory } from './NotificationBell'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

function notification(id: number, patch: Partial<Notification>): Notification {
  return {
    id,
    kind: 'request_available',
    createdAt: new Date().toISOString(),
    read: false,
    title: 'Inception',
    year: 2010,
    category: 'movies',
    requestId: 3,
    acquisitionId: null,
    seasons: [],
    reason: null,
    member: null,
    redacted: false,
    ...patch,
  }
}

const PAGE: NotificationPage = {
  unread: 2,
  nextBefore: null,
  items: [
    notification(2, {}),
    notification(1, {
      kind: 'request_rejected',
      title: 'Dark',
      year: 2017,
      seasons: [1, 2],
      reason: 'Only on disc',
    }),
  ],
}

const ATTENTION = (ids: string[]) => ({
  total: ids.length,
  sections: { decide: ids.length, fix: 0, paused: 0, setup: 0 },
  items: ids.map((id) => ({ id, kind: 'approval', section: 'decide', redacted: false })),
})

function serveAdmin(page: NotificationPage, open: string[]) {
  api.GET.mockImplementation((path: string) =>
    Promise.resolve(ok(path === '/api/v1/admin/attention' ? ATTENTION(open) : page)),
  )
}

const INCEPTION: MyRequest = {
  id: 3,
  kind: 'movie',
  titleId: '27205',
  title: 'Inception',
  year: 2010,
  poster: '/inception.jpg',
  category: 'movies',
  state: 'available',
  follow: false,
  canWithdraw: false,
  canCancel: false,
  canHide: true,
  hidden: false,
  createdAt: '2026-11-01T12:00:00Z',
  updatedAt: '2026-11-01T12:00:00Z',
  items: [{ season: null, state: 'available', journey: NO_JOURNEY }],
  openIn: [{ service: 'jellyfin', name: 'Kino', url: 'https://kino.example.org/x' }],
}

/** The member stream plus the member's requests (for titles, covers and "Open in"). */
function serveMember(page: NotificationPage, requests: MyRequest[] = [INCEPTION]) {
  api.GET.mockImplementation((path: string) =>
    Promise.resolve(ok(path === '/api/v1/requests' ? requests : page)),
  )
}

describe('NotificationBell', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    serveMember(PAGE)
  })

  it('shows the unread count and the stream with reasons', async () => {
    renderWithProviders(<NotificationBell scope="member" />)
    const bell = await screen.findByRole('button', { name: 'Notifications, 2 unread' })
    expect(bell.textContent).toContain('2')
    fireEvent.click(bell)
    expect(await screen.findByText('Ready to watch')).toBeTruthy()
    expect(screen.getByText('Dark (2017) · S01, S02')).toBeTruthy()
    expect(screen.getByText('Reason: Only on disc')).toBeTruthy()
    expect(api.GET).toHaveBeenCalledWith('/api/v1/notifications', expect.anything())
  })

  it('marks everything read in the admin stream', async () => {
    api.POST.mockResolvedValue(ok({ unread: 0 }))
    serveAdmin(PAGE, [])
    renderWithProviders(<NotificationBell scope="admin" />)
    fireEvent.click(await screen.findByRole('button', { name: 'Notifications' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Mark all read' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/notifications/read', { body: {} }),
    )
  })

  it('opening one marks it read', async () => {
    api.POST.mockResolvedValue(ok({ unread: 1 }))
    renderWithProviders(<NotificationBell scope="member" />)
    fireEvent.click(await screen.findByRole('button', { name: 'Notifications, 2 unread' }))
    fireEvent.click(await screen.findByText('Inception (2010)'))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/notifications/read', { body: { ids: [2] } }),
    )
  })

  it('counts what needs the admin, the same number as the Inbox (ADM-23)', async () => {
    serveAdmin(PAGE, ['approval:1', 'failed:2', 'stuck:3'])
    renderWithProviders(<NotificationBell scope="admin" />)
    const bell = await screen.findByRole('button', { name: 'Notifications, 3 things need you' })
    expect(bell.textContent).toContain('3')
    fireEvent.click(bell)
    expect(await screen.findByText('3 things need you')).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Open inbox' }).getAttribute('href')).toBe(
      '/admin/inbox',
    )
  })

  it('lists one entry per subject with its current state and opens the panel (ADM-24)', async () => {
    const page: NotificationPage = {
      unread: 3,
      nextBefore: null,
      items: [
        notification(9, { kind: 'acquisition_failed', acquisitionId: 5, requestId: null }),
        notification(8, { kind: 'acquisition_failed', acquisitionId: 5, requestId: null }),
        notification(7, { kind: 'approval_pending', requestId: 4, member: 'anna', title: 'Dark' }),
        notification(6, {
          kind: 'approval_pending',
          requestId: 12,
          member: null,
          title: '',
          category: 'adult',
          redacted: true,
          read: true,
        }),
      ],
    }
    serveAdmin(page, ['failed:5', 'approval:12'])
    api.POST.mockResolvedValue(ok({ unread: 1 }))
    renderWithProviders(<NotificationBell scope="admin" />, { path: '/admin/members' })
    fireEvent.click(await screen.findByRole('button', { name: /Notifications/ }))
    expect(await screen.findAllByText('Download failed')).toHaveLength(1)
    expect(screen.getByText('+1 more like it')).toBeTruthy()
    // Decided meanwhile: the state says so instead of "anna asked for this" alone.
    expect(screen.getByText('anna asked for this')).toBeTruthy()
    expect(screen.getAllByText('Done')).toHaveLength(1)
    expect(screen.getAllByText('Needs you')).toHaveLength(2)
    // A neutral private line names nobody (no subject-less "asked for this").
    expect(screen.getByText('A member asked for this')).toBeTruthy()
    expect(screen.getByText('A title in a private category')).toBeTruthy()
    fireEvent.click(screen.getByText('Download failed'))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/admin/notifications/read', {
        body: { ids: [9, 8] },
      }),
    )
  })

  it('names the outcome and puts read, decided entries under "Earlier" (slice 12)', async () => {
    const recent = new Date(Date.now() - 60 * 60 * 1000).toISOString()
    const page: NotificationPage = {
      unread: 1,
      nextBefore: null,
      items: [
        notification(3, { kind: 'approval_pending', requestId: 3, member: 'anna', title: 'Up' }),
        notification(2, {
          kind: 'approval_pending',
          requestId: 2,
          member: 'anna',
          title: 'Moon Music',
          requestState: 'rejected',
          read: true,
          createdAt: recent,
        }),
        notification(1, {
          kind: 'approval_pending',
          requestId: 1,
          member: 'anna',
          title: 'Nocturne',
          requestState: 'revoked',
          read: true,
          createdAt: '2026-01-01T10:00:00Z',
        }),
      ],
    }
    serveAdmin(page, ['approval:3'])
    renderWithProviders(<NotificationBell scope="admin" />, { path: '/admin/members' })
    fireEvent.click(await screen.findByRole('button', { name: /Notifications/ }))
    const earlier = await screen.findByRole('list', { name: 'Earlier' })
    expect(within(earlier).getByText(/Moon Music/)).toBeTruthy()
    expect(within(earlier).getByText('Declined')).toBeTruthy()
    // Read and decided more than a week ago: no longer in the bell.
    expect(screen.queryByText(/Nocturne/)).toBeNull()
    expect(screen.queryByText('Done')).toBeNull()
    expect(screen.getByText('Needs you')).toBeTruthy()
  })

  it('focuses the first notification when it opens (MEM-21)', async () => {
    serveMember(PAGE)
    renderWithProviders(<NotificationBell scope="member" />)
    fireEvent.click(await screen.findByRole('button', { name: 'Notifications, 2 unread' }))
    const first = await screen.findByRole('link', { name: /Ready to watch/ })
    await waitFor(() => expect(document.activeElement).toBe(first))
  })

  it('opens the title over its request row, with a cover and "Open in" (MEM-21)', async () => {
    serveMember(PAGE)
    const { container } = renderWithProviders(<NotificationBell scope="member" />)
    fireEvent.click(await screen.findByRole('button', { name: 'Notifications, 2 unread' }))
    const ready = await screen.findByRole('link', { name: /Ready to watch/ })
    await waitFor(() =>
      expect(ready.getAttribute('href')).toBe('/requests?request=3&title=movie-27205'),
    )
    const openIn = screen.getByRole('link', { name: /Open in Kino/ })
    expect(openIn.getAttribute('href')).toBe('https://kino.example.org/x')
    expect(openIn.getAttribute('target')).toBe('_blank')
    expect(container.ownerDocument.querySelector('img[src*="inception"]')).toBeTruthy()
    // A declined one leads to the same title, without "Open in".
    const dark = screen.getByRole('link', { name: /Declined/ })
    expect(dark.getAttribute('href')).toBe('/requests?request=3&title=movie-27205')
    expect(screen.getAllByRole('link', { name: /Open in/ })).toHaveLength(1)
    expect(screen.getByRole('link', { name: 'See all notifications' }).getAttribute('href')).toBe(
      '/profile/notifications',
    )
  })
})

describe('NotificationBell focus', () => {
  it('keeps focus on "Mark all read" after pressing it', async () => {
    serveMember(PAGE)
    api.POST.mockResolvedValue(ok({ unread: 0 }))
    renderWithProviders(<NotificationBell scope="member" />)
    fireEvent.click(await screen.findByRole('button', { name: 'Notifications, 2 unread' }))
    const markAll = await screen.findByRole('button', { name: 'Mark all read' })
    act(() => markAll.focus())
    serveMember({ ...PAGE, unread: 0, items: PAGE.items.map((n) => ({ ...n, read: true })) })
    fireEvent.click(markAll)
    await waitFor(() => expect(api.POST).toHaveBeenCalled())
    await waitFor(() => expect(markAll.getAttribute('aria-disabled')).toBe('true'))
    expect(document.activeElement).toBe(markAll)
  })
})

describe('NotificationHistory', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
  })

  it('lists the whole stream page by page and focuses the first new one', async () => {
    const first: NotificationPage = {
      unread: 1,
      nextBefore: 2,
      items: [
        notification(3, { title: 'Arrival', requestId: 9 }),
        notification(2, { title: 'Dune', requestId: 8, read: true }),
      ],
    }
    const second: NotificationPage = {
      unread: 1,
      nextBefore: null,
      items: [notification(1, { title: 'Tenet', requestId: 7, read: true })],
    }
    api.GET.mockImplementation(
      (path: string, init?: { params?: { query?: { before?: number } } }) =>
        Promise.resolve(
          ok(path === '/api/v1/requests' ? [] : init?.params?.query?.before === 2 ? second : first),
        ),
    )
    renderWithProviders(<NotificationHistory />)
    expect(await screen.findByText('Arrival (2010)')).toBeTruthy()
    expect(screen.queryByText('Tenet (2010)')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Show older' }))
    const tenet = await screen.findByRole('link', { name: /Tenet/ })
    await waitFor(() => expect(document.activeElement).toBe(tenet))
    expect(screen.queryByRole('button', { name: 'Show older' })).toBeNull()
    expect(tenet.getAttribute('href')).toBe('/requests?request=7')
  })

  it('keeps the loaded list when older ones fail to load', async () => {
    const first: NotificationPage = {
      unread: 0,
      nextBefore: 2,
      items: [notification(3, { title: 'Arrival', requestId: 9, read: true })],
    }
    api.GET.mockImplementation(
      (path: string, init?: { params?: { query?: { before?: number } } }) =>
        path === '/api/v1/requests'
          ? Promise.resolve(ok([]))
          : init?.params?.query?.before === 2
            ? Promise.resolve(fail('unavailable', 503))
            : Promise.resolve(ok(first)),
    )
    renderWithProviders(<NotificationHistory />)
    const older = await screen.findByRole('button', { name: 'Show older' })
    act(() => older.focus())
    fireEvent.click(older)
    expect(await screen.findByText(/Older notifications couldn't be loaded/)).toBeTruthy()
    expect(screen.getByText('Arrival (2010)')).toBeTruthy()
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Show older' }))
  })
})
