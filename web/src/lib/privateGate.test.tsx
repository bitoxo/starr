import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query'
import { act, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router'
import { toast } from 'sonner'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError } from '@/api/errors'
import { privateKey } from '@/api/private'
import { useSetPrivateUnlock } from '@/api/privacy'
import { ME_KEY } from '@/auth/session'
import { ok } from '@/test/render'

import { dismissPrivateToasts, gateClosedOf, privateToast, usePrivateGate } from './privateGate'

const api = vi.hoisted(() => ({ GET: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const NOW = Date.parse('2026-11-06T20:44:00Z')
const UNTIL = '2026-11-06T21:14:00Z'

const me = (unlocked: boolean, until: string | null = unlocked ? UNTIL : null) => ({
  kind: 'member',
  username: 'cara',
  displayName: 'Cara',
  privateCategory: { unlocked, unlockedUntil: until },
})

let state = { unlocked: true }

function serveMe() {
  api.GET.mockImplementation((path: string) => {
    // The server's gate: open until the unlock time, unless locked.
    if (path === '/api/v1/auth/me')
      return Promise.resolve(ok(me(state.unlocked && Date.now() < Date.parse(UNTIL))))
    return Promise.resolve(ok({}))
  })
}

function Probe() {
  const location = useLocation()
  return (
    <output data-testid="location" data-notice={gateClosedOf(location.state) ?? ''}>
      {location.pathname}
    </output>
  )
}

/** A private page: mounted only under `/private`, like the area's routes. */
function PrivatePage() {
  useQuery({ queryKey: privateKey('scene', 'a'), queryFn: () => ({ title: 'Scene A' }) })
  return null
}

/** The watcher plus a private page and a notification page, as a shell would hold them. */
function Shell({ onClose }: { onClose?: (close: () => void) => void }) {
  usePrivateGate('/')
  const lock = useSetPrivateUnlock()
  onClose?.(() => lock.mutate(false))
  const { pathname } = useLocation()
  return (
    <>
      {pathname.startsWith('/private') && <PrivatePage />}
      <Probe />
    </>
  )
}

function setup(path = '/private/scenes/a') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  })
  client.setQueryData(ME_KEY, me(true))
  client.setQueryData(['notifications', 'member'], {
    unread: 2,
    nextBefore: null,
    items: [
      { id: 1, category: 'adult', title: 'Scene A' },
      { id: 2, category: 'movies', title: 'Arrival' },
    ],
  })
  client.setQueryData(['notifications', 'member', 'history'], {
    pageParams: [null],
    pages: [
      {
        unread: 2,
        nextBefore: null,
        items: [
          { id: 1, category: 'adult', title: 'Scene A' },
          { id: 2, category: 'movies', title: 'Arrival' },
        ],
      },
    ],
  })
  client.setQueryData(['admin', 'attention'], { total: 1, sections: {}, items: [] })
  let close: () => void = () => undefined
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <Shell onClose={(c) => (close = c)} />
      </MemoryRouter>
    </QueryClientProvider>,
  )
  return { client, view, close: () => close() }
}

const location = () => screen.getByTestId('location')

describe('usePrivateGate', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true, now: NOW })
    state = { unlocked: true }
    api.GET.mockReset()
    api.PUT.mockReset()
    serveMe()
  })
  afterEach(() => {
    vi.useRealTimers()
    dismissPrivateToasts()
  })

  it('closes when the unlock time passes: queries, notification entries, toasts, page', async () => {
    const { client } = setup()
    privateToast('Scene requested – waiting for approval')
    toast('Profile saved', { id: 'general' })
    await waitFor(() => expect(client.getQueryData(privateKey('scene', 'a'))).toBeDefined())

    // No activity: the server closes the unlock at its time.
    await act(() => vi.advanceTimersByTimeAsync(30 * 60_000 + 2_000))

    await waitFor(() => expect(location().textContent).toBe('/'))
    expect(location().dataset.notice).toBe('idle')
    expect(client.getQueryCache().findAll({ queryKey: privateKey() })).toHaveLength(0)
    expect(client.getQueryData(['admin', 'attention'])).toBeUndefined()
    const page = client.getQueryData<{ items: { category: string }[] }>(['notifications', 'member'])
    expect(page?.items.map((n) => n.category)).toEqual(['movies'])
    // The full list on the profile is paged: each page loses its private entries too.
    const history = client.getQueryData<{ pages: { items: { category: string }[] }[] }>([
      'notifications',
      'member',
      'history',
    ])
    expect(history?.pages[0]?.items.map((n) => n.category)).toEqual(['movies'])
    const ids = toast.getToasts().map((t) => t.id)
    expect(ids).toContain('general')
    expect(ids.some((id) => String(id).startsWith('private:'))).toBe(false)
  })

  it('reads the gate again every minute while unlocked and on focus', async () => {
    setup()
    const calls = () => api.GET.mock.calls.filter(([p]) => p === '/api/v1/auth/me').length
    const before = calls()
    await act(() => vi.advanceTimersByTimeAsync(60_000))
    await waitFor(() => expect(calls()).toBeGreaterThan(before))
    const afterMinute = calls()
    act(() => void window.dispatchEvent(new Event('focus')))
    await waitFor(() => expect(calls()).toBeGreaterThan(afterMinute))
  })

  it('closes at once when another tab locked (focus or broadcast), with a neutral notice', async () => {
    setup()
    state.unlocked = false
    act(() => void window.dispatchEvent(new Event('focus')))
    await waitFor(() => expect(location().textContent).toBe('/'))
    expect(location().dataset.notice).toBe('elsewhere')
  })

  it('a lock in another tab reaches this one through the broadcast channel', async () => {
    setup()
    state.unlocked = false
    const other = new BroadcastChannel('starr:private-gate')
    other.postMessage('changed')
    other.close()
    await waitFor(() => expect(location().textContent).toBe('/'))
  })

  it('forgets everything private when the session ends elsewhere, without a notice', async () => {
    const { client } = setup()
    privateToast('Scene requested – waiting for approval')
    await waitFor(() => expect(client.getQueryData(privateKey('scene', 'a'))).toBeDefined())
    api.GET.mockImplementation(() =>
      Promise.resolve({
        data: undefined,
        error: undefined,
        response: new Response(null, { status: 401 }),
      }),
    )
    act(() => void window.dispatchEvent(new Event('focus')))
    await waitFor(() => expect(client.getQueryData(ME_KEY)).toBeNull())
    await waitFor(() =>
      expect(client.getQueryCache().findAll({ queryKey: privateKey() })).toHaveLength(0),
    )
    expect(toast.getToasts().some((t) => String(t.id).startsWith('private:'))).toBe(false)
    expect(location().dataset.notice).toBe('')
  })

  it('a private 404 reads the gate again', async () => {
    const { client } = setup()
    state.unlocked = false
    await act(() =>
      client
        .fetchQuery({
          queryKey: privateKey('scene', 'b'),
          queryFn: () => Promise.reject(new ApiError('private.not_found', 404)),
        })
        .catch(() => undefined),
    )
    await waitFor(() => expect(location().textContent).toBe('/'))
  })

  it('closing it yourself forgets everything at once, without a notice', async () => {
    api.PUT.mockResolvedValue(ok({ unlocked: false, unlockedUntil: null }))
    const { client, close } = setup()
    privateToast('Following – new scenes will be requested')
    state.unlocked = false
    act(() => close())
    await waitFor(() => expect(location().textContent).toBe('/'))
    expect(location().dataset.notice).toBe('')
    expect(client.getQueryCache().findAll({ queryKey: privateKey() })).toHaveLength(0)
    expect(toast.getToasts().some((t) => String(t.id).startsWith('private:'))).toBe(false)
  })

  it('outside the private area, closing it yourself stays on the page', async () => {
    api.PUT.mockResolvedValue(ok({ unlocked: false, unlockedUntil: null }))
    const { close } = setup('/requests')
    state.unlocked = false
    act(() => close())
    await waitFor(() => expect(api.PUT).toHaveBeenCalled())
    await act(() => vi.advanceTimersByTimeAsync(10))
    expect(location().textContent).toBe('/requests')
  })
})

describe('private toasts', () => {
  it('carry the private prefix and only they are dismissed', () => {
    const id = privateToast('Scene requested')
    toast('Other', { id: 'other' })
    expect(id.startsWith('private:')).toBe(true)
    dismissPrivateToasts()
    const ids = toast.getToasts().map((t) => t.id)
    expect(ids).toContain('other')
    expect(ids).not.toContain(id)
  })
})
