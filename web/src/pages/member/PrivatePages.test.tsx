import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { Link, Route, Routes, useLocation } from 'react-router'
import { toast } from 'sonner'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type {
  PrivateArea,
  PrivateCard,
  PrivateFollow,
  PrivatePage,
  PrivateScene,
  PrivateSearch,
} from '@/api/private'
import { PrivateSearchPill } from '@/components/private/PrivateSearchPill'
import { Toaster } from '@/components/ui/sonner'
import { NotFound } from '@/pages/ErrorPage'
import { fail, LOCKED_MEMBER, ok, renderWithProviders, UNLOCKED_MEMBER } from '@/test/render'

import { PrivateLayout } from './PrivateLayout'
import {
  PrivateEntityPage,
  PrivateHomePage,
  PrivateScenePage,
  PrivateSearchPage,
} from './PrivatePages'
import { PrivateFollowingPage, PrivateRequestsPage } from './PrivateRequestsPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn(), DELETE: vi.fn(), PUT: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const AREA: PrivateArea = { ready: true, requestable: false, lists: ['new', 'trending'] }
const IMAGE = '1a6e0000-0000-4000-8000-000000000002'
const STUDIO = '57d10000-0000-4000-8000-0000000000aa'
const PERFORMER = '9e7f0000-0000-4000-8000-0000000000aa'
const SCENE_A = '5ce0e000-0000-4000-8000-00000000000a'
const SCENE_B = '5ce0e000-0000-4000-8000-00000000000b'
const SCENE_C = '5ce0e000-0000-4000-8000-00000000000c'
const CARD: PrivateCard = {
  kind: 'scene',
  id: SCENE_A,
  title: 'Scene A',
  date: '2026-10-30',
  image: IMAGE,
  studio: { kind: 'studio', id: STUDIO, name: 'Studio S' },
  performers: [{ kind: 'performer', id: PERFORMER, name: 'Performer P' }],
  durationSeconds: 1800,
  status: 'new',
  requestedByMe: false,
}
const LANE: PrivatePage = { items: [CARD], next: null }
const SCENE: PrivateScene = {
  id: SCENE_A,
  title: 'Scene A',
  date: '2026-10-30',
  durationSeconds: 3725,
  image: IMAGE,
  studio: { kind: 'studio', id: STUDIO, name: 'Studio S' },
  performers: [{ kind: 'performer', id: PERFORMER, name: 'Performer P' }],
  status: 'new',
  requestedByMe: false,
  journey: null,
  openIn: [],
  requestable: false,
  moreFromStudio: [],
}
const STUDIO_CARD: PrivateCard = {
  kind: 'studio',
  id: STUDIO,
  title: 'Studio S',
  image: null,
  performers: [],
  requestedByMe: false,
}
const PERFORMER_CARD: PrivateCard = {
  ...STUDIO_CARD,
  kind: 'performer',
  id: PERFORMER,
  title: 'Performer P',
}
const FOLLOW: PrivateFollow = {
  id: 7,
  kind: 'studio',
  entityId: STUDIO,
  name: 'Studio S',
  image: null,
  knownThrough: '2026-10-01',
  createdAt: '2026-10-01T12:00:00Z',
  checkedAt: '2026-11-05T12:00:00Z',
  cappedScenes: null,
  weeklyCap: 10,
  remainingThisWeek: 7,
}

function search(over: Partial<PrivateSearch> = {}): PrivateSearch {
  return {
    scenes: [CARD],
    performers: [],
    studios: [],
    next: null,
    counts: { scenes: 1, performers: 0, studios: 0 },
    ...over,
  }
}

/** The router's current location, as the address bar would show it. */
function LocationProbe() {
  const { pathname, search, hash } = useLocation()
  return <output data-testid="location">{`${pathname}${search}${hash}`}</output>
}

function area(path: string, me: unknown = UNLOCKED_MEMBER) {
  return renderWithProviders(
    <>
      <LocationProbe />
      <Routes>
        <Route path="/private" element={<PrivateLayout />}>
          <Route index element={<PrivateHomePage />} />
          <Route path="search" element={<PrivateSearchPage />} />
          <Route path="requests" element={<PrivateRequestsPage />} />
          <Route path="following" element={<PrivateFollowingPage />} />
          <Route path="scenes/:id" element={<PrivateScenePage />} />
          <Route path="performers/:id" element={<PrivateEntityPage kind="performer" />} />
          <Route path="studios/:id" element={<PrivateEntityPage kind="studio" />} />
        </Route>
        <Route path="*" element={<NotFound />} />
      </Routes>
    </>,
    { path, me },
  )
}

function location(): string {
  return screen.getByTestId('location').textContent ?? ''
}

/** The page's markup without the location probe. */
function page(container: HTMLElement): string {
  const copy = container.cloneNode(true) as HTMLElement
  copy.querySelector('[data-testid="location"]')?.remove()
  return copy.innerHTML
}

/** The newest toast is the given neutral text under the private id prefix (spec 0008 F2). */
function expectNeutralPrivateToast(text: string) {
  const shown = toast.getToasts().at(-1) as { id: string | number; title?: unknown } | undefined
  expect(shown?.title).toBe(text)
  expect(String(shown?.id)).toMatch(/^private:/)
  expect(JSON.stringify(shown?.title)).not.toMatch(/Scene A|Performer P|Studio S/)
}

/** Answer GETs by path; anything not listed is the area. */
function answer(routes: Record<string, unknown>) {
  api.GET.mockImplementation((path: string) =>
    Promise.resolve(ok(path in routes ? routes[path] : AREA)),
  )
}

describe('private area', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    api.DELETE.mockReset()
    toast.dismiss()
  })

  it('is the same "not found" as any unknown address while locked, without asking', async () => {
    // Spec 0008 PRIV-03: byte-identical markup, no loading state, no request.
    const accounts = [LOCKED_MEMBER, { ...LOCKED_MEMBER, privateCategory: null }]
    for (const me of accounts) {
      const unknown = area('/nope', me)
      const expected = page(unknown.container)
      unknown.unmount()
      for (const path of [
        '/private',
        '/private/search?q=Scene',
        '/private/requests',
        '/private/following',
        `/private/scenes/${SCENE_A}`,
        '/private/performers/x',
        `/private/studios/${STUDIO}`,
      ]) {
        const view = area(path, me)
        expect(screen.getByText('Nothing out here')).toBeTruthy()
        expect(page(view.container)).toBe(expected)
        expect(view.container.querySelector('img')).toBeNull()
        view.unmount()
      }
    }
    expect(api.GET).not.toHaveBeenCalled()
    expect(api.POST).not.toHaveBeenCalled()
  })

  it('is plain "not found" when the server says the gate closed: no name, no image', async () => {
    api.GET.mockResolvedValue(fail('private.not_found', 404))
    const { container } = area('/private')
    expect(await screen.findByText('Nothing out here')).toBeTruthy()
    expect(container.querySelector('img')).toBeNull()
    expect(page(container)).not.toMatch(/private|adult/i)
    expect(api.GET).toHaveBeenCalledTimes(1)
    expect(api.GET).toHaveBeenCalledWith('/api/v1/private')
  })

  it('browses Trending by default as a 16:9 grid, New on the pill, the list in the URL (W5, PRIV-11)', async () => {
    answer({ '/api/v1/private/discover/{list}': LANE })
    const { container } = area('/private')
    const grid = await screen.findByRole('list', { name: 'Trending scenes' })
    const tile = await within(grid).findByRole('link', { name: /Scene A/ })
    expect(tile.getAttribute('href')).toBe(`/private/scenes/${SCENE_A}`)
    // Name: title, studio · performers, date, running time ("30 min", never "30:00").
    expect(tile.getAttribute('aria-label')).toBe(
      'Scene A, Studio S · Performer P, Oct 30, 2026, 30 min',
    )
    expect(api.GET).toHaveBeenCalledWith('/api/v1/private/discover/{list}', {
      params: { path: { list: 'trending' }, query: { page: 1 } },
    })
    const img = container.querySelector('img')
    expect(img?.getAttribute('src')).toBe(`/api/v1/images/stashdb/600/${IMAGE}`)
    expect(img?.className).toContain('aspect-video')
    expect(screen.getByText(/Requests are not open in this area yet/)).toBeTruthy()
    // Not requestable: nothing to select.
    expect(screen.queryByRole('checkbox')).toBeNull()

    fireEvent.click(screen.getByRole('button', { name: 'New' }))
    expect(await screen.findByRole('list', { name: 'New scenes' })).toBeTruthy()
    expect(location()).toBe('/private?list=new')
    await waitFor(() =>
      expect(api.GET).toHaveBeenCalledWith('/api/v1/private/discover/{list}', {
        params: { path: { list: 'new' }, query: { page: 1 } },
      }),
    )
  })

  it('is one label everywhere: "Private", never NSFW or "private category" (PRIV-15)', async () => {
    answer({ '/api/v1/private/discover/{list}': LANE })
    const { container } = area('/private')
    await screen.findByRole('list', { name: 'Trending scenes' })
    expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Private')
    expect(container.textContent).not.toMatch(/NSFW|private category/i)
  })

  it('explains when no catalog is connected', async () => {
    api.GET.mockResolvedValue(ok({ ready: false, requestable: false, lists: [] }))
    area('/private')
    expect(await screen.findByText('Nothing to browse yet')).toBeTruthy()
  })

  it('searches with the text only in the request body; studios, then performers, then scenes with counts (PRIV-14)', async () => {
    answer({ '/api/v1/private/follows': [FOLLOW] })
    api.POST.mockResolvedValue(
      ok(
        search({
          studios: [STUDIO_CARD],
          performers: [PERFORMER_CARD],
          counts: { scenes: 128, performers: 1, studios: 1 },
        }),
      ),
    )
    area('/private/search?q=Needle')
    expect(await screen.findByRole('heading', { name: 'Results for “Needle”' })).toBeTruthy()
    await screen.findByRole('heading', { name: 'Studios 1' })
    const headings = screen.getAllByRole('heading', { level: 2 }).map((h) => h.textContent)
    expect(headings).toEqual(['Studios 1', 'Performers 1', 'Scenes 128'])
    expect(api.POST).toHaveBeenCalledWith('/api/v1/private/search', { body: { query: 'Needle' } })
    // The followed studio says so.
    await waitFor(() =>
      expect(
        within(screen.getByRole('list', { name: 'Studios 1' })).getByText('Following'),
      ).toBeTruthy(),
    )
    // No request URL carries the text: only the page's own address under /private.
    for (const call of [...api.GET.mock.calls, ...api.POST.mock.calls]) {
      expect(String(call[0])).not.toMatch(/Needle/)
    }
    expect(location()).toBe('/private/search?q=Needle')
  })

  it('narrows the scenes to a studio from its lane, with a removable chip (Q7 C)', async () => {
    answer({ '/api/v1/private/follows': [] })
    api.POST.mockResolvedValue(
      ok(search({ studios: [STUDIO_CARD], counts: { scenes: 3, performers: 0, studios: 1 } })),
    )
    area('/private/search?q=Scene')
    fireEvent.click(await screen.findByRole('button', { name: 'Show scenes of Studio S' }))
    await waitFor(() => expect(location()).toBe(`/private/search?q=Scene&studio=${STUDIO}`))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/private/search', {
        body: { query: 'Scene', studio: STUDIO },
      }),
    )
    await waitFor(() =>
      expect(
        screen
          .getByRole('button', { name: 'Show scenes of Studio S' })
          .getAttribute('aria-pressed'),
      ).toBe('true'),
    )
    // The chip opens the studio page and removes the filter.
    expect(screen.getByRole('link', { name: 'Open Studio S' }).getAttribute('href')).toBe(
      `/private/studios/${STUDIO}`,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Studio S' }))
    await waitFor(() => expect(location()).toBe('/private/search?q=Scene'))
  })

  it('has scene-specific empty states (PRIV-23)', async () => {
    answer({ '/api/v1/private/follows': [] })
    api.POST.mockResolvedValue(
      ok(search({ scenes: [], counts: { scenes: 0, performers: 0, studios: 0 } })),
    )
    area('/private/search?q=zzzz')
    expect(await screen.findByText('Nothing matches “zzzz”')).toBeTruthy()
    expect(screen.getByText(/No scene, performer or studio has that name/)).toBeTruthy()
    expect(screen.queryByText(/original title/)).toBeNull()
    expect(screen.getByRole('link', { name: 'Back to browsing' }).getAttribute('href')).toBe(
      '/private',
    )
  })

  it('shows tiles in their final shape while searching, and long titles in full on hover (PRIV-24)', async () => {
    answer({ '/api/v1/private/follows': [] })
    let resolve: (value: unknown) => void = () => {}
    api.POST.mockReturnValue(new Promise((r) => (resolve = r)))
    const { container } = area('/private/search?q=x')
    const loading = await screen.findByRole('list', { name: /Scenes/ })
    expect(loading.getAttribute('aria-busy')).toBe('true')
    expect(loading.querySelectorAll('[data-skeleton]').length).toBeGreaterThan(6)
    const long = { ...CARD, image: null, title: 'Scene A '.repeat(12).trim() }
    resolve(ok(search({ scenes: [long] })))
    await waitFor(() => expect(container.querySelector('[data-cover-fallback]')).toBeTruthy())
    const fallback = container.querySelector('[data-cover-fallback]')
    expect(fallback?.getAttribute('title')).toBe(long.title)
    expect(fallback?.innerHTML).toContain('line-clamp-2')
  })

  it('shows a scene with its studio, performers, date and running time; Back goes home without history', async () => {
    answer({ '/api/v1/private/scenes/{sceneId}': SCENE })
    area(`/private/scenes/${SCENE_A}`)
    expect(await screen.findByRole('heading', { name: 'Scene A' })).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Performer P' }).getAttribute('href')).toBe(
      `/private/performers/${PERFORMER}`,
    )
    expect(screen.getByRole('link', { name: 'Studio S' }).getAttribute('href')).toBe(
      `/private/studios/${STUDIO}`,
    )
    // PRIV-06: words, never "1:02:05", in the facts line beside the date.
    const facts = screen.getByText('Oct 30, 2026').closest('p')
    expect(facts?.textContent).toMatch(/Oct 30, 2026·1 h 2 min/)
    expect(document.body.textContent).not.toMatch(/\d:\d\d/)
    fireEvent.click(screen.getByRole('link', { name: 'Back' }))
    await waitFor(() => expect(location()).toBe('/private'))
  })

  it('goes back in the history when there is an in-app entry (PRIV-13)', async () => {
    answer({
      '/api/v1/private/scenes/{sceneId}': SCENE,
      '/api/v1/private/follows': [],
    })
    api.POST.mockResolvedValue(ok(search()))
    area('/private/search?q=Scene')
    fireEvent.click(await screen.findByRole('link', { name: /^Scene A, Studio S/ }))
    await screen.findByRole('heading', { name: 'Scene A' })
    fireEvent.click(screen.getByRole('link', { name: 'Back' }))
    await waitFor(() => expect(location()).toBe('/private/search?q=Scene'))
  })

  it('requests a scene from its page with a neutral toast that names the outcome and links to the requests (PRIV-21)', async () => {
    let requested = false
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(
        path === '/api/v1/private'
          ? ok({ ...AREA, requestable: true })
          : ok({
              ...SCENE,
              requestable: true,
              status: requested ? 'requested' : 'new',
              requestedByMe: requested,
            }),
      ),
    )
    api.POST.mockImplementation(() => {
      requested = true
      return Promise.resolve(ok({ requests: [{ id: 9, state: 'approved' }], skipped: [] }, 201))
    })
    area(`/private/scenes/${SCENE_A}`)
    const button = await screen.findByRole('button', { name: 'Request scene' })
    act(() => button.focus())
    fireEvent.click(button)
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/private/requests', {
        body: { scenes: [SCENE_A] },
      }),
    )
    expect(await screen.findByText('Requested')).toBeTruthy()
    // The button gave way: focus stays in the request card, never on <body> (F3).
    await waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByRole('region', { name: 'Request and status' }),
      ),
    )
    expectNeutralPrivateToast('Scene requested – approved automatically')
    const shown = toast.getToasts().at(-1) as { action?: { label: string; onClick: () => void } }
    expect(shown.action?.label).toBe('Your requests')
    act(() => shown.action?.onClick())
    await waitFor(() => expect(location()).toBe('/private/requests'))
  })

  it('selects scenes with the checkbox or Space, says "n / 20", Esc clears, and requests them together (PRIV-18)', async () => {
    const scenes = [
      CARD,
      { ...CARD, id: SCENE_B, title: 'Scene B' },
      { ...CARD, id: SCENE_C, title: 'Scene C', status: 'requested' as const, requestedByMe: true },
    ]
    answer({
      '/api/v1/private': { ...AREA, requestable: true },
      '/api/v1/private/performers/{performerId}': {
        kind: 'performer',
        id: PERFORMER,
        name: 'Performer P',
        image: null,
        scenes,
        requestable: true,
      },
    })
    api.POST.mockResolvedValue(
      ok(
        {
          requests: [
            { id: 1, state: 'pending_approval' },
            { id: 2, state: 'pending_approval' },
          ],
          skipped: [],
        },
        201,
      ),
    )
    area(`/private/performers/${PERFORMER}`)
    const box = await screen.findByRole('checkbox', { name: 'Select Scene A' })
    // The checkbox comes before the scene's link in the DOM; it is not a tab stop of its own.
    const link = screen.getByRole('link', { name: /^Scene A,/ })
    expect(box.compareDocumentPosition(link) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
    expect(box.getAttribute('tabindex')).toBe('-1')
    // Already requested: nothing to pick. The performer itself is never requested.
    expect(screen.queryByRole('checkbox', { name: 'Select Scene C' })).toBeNull()
    expect(screen.queryByRole('button', { name: /Request.*Performer P/ })).toBeNull()

    fireEvent.click(box)
    fireEvent.keyDown(screen.getByRole('link', { name: /^Scene B,/ }), { key: ' ' })
    const bar = screen.getByRole('region', { name: '2 selected' })
    expect(within(bar).getByText(/20 selected/)).toBeTruthy()
    expect(screen.getByRole('link', { name: /^Scene B, .*selected$/ })).toBeTruthy()
    // Esc clears.
    fireEvent.keyDown(document, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('region', { name: /selected/ })).toBeNull())

    fireEvent.click(screen.getByRole('checkbox', { name: 'Select Scene A' }))
    fireEvent.keyDown(screen.getByRole('link', { name: /^Scene B,/ }), { key: ' ' })
    fireEvent.click(
      within(screen.getByRole('region', { name: '2 selected' })).getByRole('button', {
        name: 'Request 2 scenes',
      }),
    )
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/private/requests', {
        body: { scenes: [SCENE_A, SCENE_B] },
      }),
    )
    await waitFor(() => expect(screen.queryByRole('region', { name: /selected/ })).toBeNull())
    // The bar went with its buttons: focus is back in the grid, not on <body> (F3).
    expect(document.activeElement?.getAttribute('aria-label')).toMatch(/^Scene /)
    expectNeutralPrivateToast('2 scenes requested – waiting for approval')
  })

  it('moves through the scene grid with the arrow keys as one tab stop (F4)', async () => {
    const scenes = [CARD, { ...CARD, id: SCENE_B, title: 'Scene B' }]
    answer({ '/api/v1/private/discover/{list}': { items: scenes, next: null } })
    area('/private')
    const first = await screen.findByRole('link', { name: /^Scene A,/ })
    const second = screen.getByRole('link', { name: /^Scene B,/ })
    expect([first.tabIndex, second.tabIndex]).toEqual([0, -1])
    first.focus()
    fireEvent.keyDown(first, { key: 'ArrowRight' })
    expect(document.activeElement).toBe(second)
  })

  it('shows a studio as a logo tile whose scenes name the performers (PRIV-22, PRIV-25)', async () => {
    answer({
      '/api/v1/private/studios/{studioId}': {
        kind: 'studio',
        id: STUDIO,
        name: 'Studio S',
        image: IMAGE,
        scenes: [CARD],
        requestable: false,
        follow: FOLLOW,
      },
    })
    const { container } = area(`/private/studios/${STUDIO}`)
    expect(await screen.findByRole('heading', { name: 'Studio S' })).toBeTruthy()
    const logo = container.querySelector(
      `img[src="/api/v1/images/stashdb/600/${IMAGE}"].object-contain`,
    )
    expect(logo).toBeTruthy()
    expect(screen.getAllByText('Following').length).toBeGreaterThan(0)
    expect(screen.getByText('Up to 10 new scenes per week (7 left this week)')).toBeTruthy()
    const tile = screen.getByRole('link', { name: /^Scene A,/ })
    expect(tile.getAttribute('aria-label')).toMatch(/^Scene A, Performer P,/)
    expect(tile.getAttribute('aria-label')).not.toMatch(/Studio S/)
  })

  it('shows a performer round, with a monogram without a portrait (PRIV-22)', async () => {
    answer({
      '/api/v1/private/performers/{performerId}': {
        kind: 'performer',
        id: PERFORMER,
        name: 'Performer P',
        image: null,
        scenes: [CARD],
        requestable: false,
      },
    })
    const { container } = area(`/private/performers/${PERFORMER}`)
    expect(await screen.findByRole('heading', { name: 'Performer P' })).toBeTruthy()
    expect(container.querySelector('.rounded-full.aspect-square')?.textContent).toBe('P')
  })

  it('follows a performer from its page', async () => {
    answer({
      '/api/v1/private': { ...AREA, requestable: true },
      '/api/v1/private/performers/{performerId}': {
        kind: 'performer',
        id: PERFORMER,
        name: 'Performer P',
        image: null,
        scenes: [CARD],
        requestable: true,
        follow: null,
      },
    })
    api.POST.mockResolvedValue(ok({ id: 1 }, 201))
    area(`/private/performers/${PERFORMER}`)
    expect(
      await screen.findByRole('heading', { name: 'Follow new scenes of this performer' }),
    ).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Follow' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/private/follows', {
        body: { kind: 'performer', id: PERFORMER },
      }),
    )
    await waitFor(() => expectNeutralPrivateToast('Following – new scenes are requested for you'))
  })

  it('lists follows with the weekly cap in numbers and a link to what the cap left (PRIV-09)', async () => {
    answer({
      '/api/v1/private/follows': [
        FOLLOW,
        {
          ...FOLLOW,
          id: 8,
          kind: 'performer',
          entityId: PERFORMER,
          name: 'Performer P',
          cappedScenes: 2,
          remainingThisWeek: 0,
        },
      ],
    })
    area('/private/following')
    expect(await screen.findByText('Up to 10 new scenes per week (7 left this week)')).toBeTruthy()
    expect(screen.getByText('Up to 10 new scenes per week (0 left this week)')).toBeTruthy()
    expect(screen.getByText('Weekly limit reached')).toBeTruthy()
    expect(screen.getByText('2 newer scenes not requested')).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Show them' }).getAttribute('href')).toBe(
      `/private/performers/${PERFORMER}`,
    )
    expect(screen.getAllByRole('link', { name: /Studio S/ })[0]?.getAttribute('href')).toBe(
      `/private/studios/${STUDIO}`,
    )
  })

  it('unfollows at once with an undo that restores the original cutoff (PRIV-17)', async () => {
    // After the unfollow the list no longer has it: the undo still reports back.
    let following = true
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(ok(path === '/api/v1/private/follows' ? (following ? [FOLLOW] : []) : AREA)),
    )
    api.DELETE.mockImplementation(() => {
      following = false
      return Promise.resolve(ok(undefined, 204))
    })
    api.POST.mockImplementation(() => {
      following = true
      return Promise.resolve(ok(FOLLOW))
    })
    area('/private/following')
    renderWithProviders(<Toaster />, { router: false })
    fireEvent.click(await screen.findByRole('button', { name: 'Stop following Studio S' }))
    await waitFor(() =>
      expect(api.DELETE).toHaveBeenCalledWith('/api/v1/follows/{id}', {
        params: { path: { id: 7 } },
      }),
    )
    const undo = await screen.findByRole('button', { name: 'Undo' })
    expect(String(toast.getToasts().at(-1)?.id)).toMatch(/^private:/)
    expect(screen.getByText('No longer following')).toBeTruthy()
    fireEvent.click(undo)
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/private/follows/{id}/restore', {
        params: { path: { id: 7 } },
      }),
    )
    await waitFor(() => expectNeutralPrivateToast('Following again, from the original date'))
  })

  it('says so when the undo of an unfollow fails', async () => {
    let first = true
    api.GET.mockImplementation((path: string) => {
      if (path !== '/api/v1/private/follows') return Promise.resolve(ok(AREA))
      const list = first ? [FOLLOW] : []
      first = false
      return Promise.resolve(ok(list))
    })
    api.DELETE.mockResolvedValue(ok(undefined, 204))
    api.POST.mockResolvedValue(fail('follows.followed_again', 409))
    area('/private/following')
    renderWithProviders(<Toaster />, { router: false })
    fireEvent.click(await screen.findByRole('button', { name: 'Stop following Studio S' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Undo' }))
    await waitFor(() =>
      expect((toast.getToasts().at(-1) as { title?: unknown } | undefined)?.title).toBe(
        'You follow it again already, from today.',
      ),
    )
    expect(String(toast.getToasts().at(-1)?.id)).toMatch(/^private:/)
  })

  it('drops the selection when the search changes', async () => {
    answer({ '/api/v1/private': { ...AREA, requestable: true }, '/api/v1/private/follows': [] })
    api.POST.mockResolvedValue(
      ok(search({ studios: [STUDIO_CARD], counts: { scenes: 1, performers: 0, studios: 1 } })),
    )
    area('/private/search?q=Scene')
    fireEvent.click(await screen.findByRole('checkbox', { name: 'Select Scene A' }))
    expect(screen.getByRole('region', { name: '1 selected' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Show scenes of Studio S' }))
    await waitFor(() => expect(screen.queryByRole('region', { name: /selected/ })).toBeNull())
  })

  it('lists scene requests on their own page with filters in the URL and scene copy (PRIV-10, PRIV-16)', async () => {
    const request = (id: number, state: string, title: string) => ({
      id,
      kind: 'scene',
      category: 'adult',
      title,
      titleId: SCENE_A,
      poster: IMAGE,
      state,
      stage: null,
      items: [{ state, journey: { steps: [] } }],
      openIn: [],
      createdAt: '2026-11-05T10:00:00Z',
      updatedAt: '2026-11-05T10:00:00Z',
      follow: false,
      hidden: false,
      canWithdraw: false,
      canCancel: state === 'approved',
      canHide: false,
    })
    answer({
      '/api/v1/private/requests': [
        request(1, 'pending_approval', 'Scene A'),
        request(2, 'approved', 'Scene B'),
      ],
    })
    area('/private/requests')
    expect(await screen.findByRole('heading', { name: 'Scene A' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: /^On its way/ }))
    await waitFor(() => expect(location()).toBe('/private/requests?state=active'))
    expect(screen.queryByRole('heading', { name: 'Scene A' })).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Cancel the request for Scene B' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Cancel this scene request?')).toBeTruthy()
    expect(dialog.textContent).not.toMatch(/season/i)
  })

  it('marks and focuses the request a private notification points to (?request=)', async () => {
    const request = {
      id: 2,
      kind: 'scene',
      category: 'adult',
      title: 'Scene B',
      titleId: SCENE_B,
      poster: IMAGE,
      state: 'available',
      stage: null,
      items: [{ state: 'available', journey: { steps: [] } }],
      openIn: [],
      createdAt: '2026-11-05T10:00:00Z',
      updatedAt: '2026-11-05T10:00:00Z',
      follow: false,
      hidden: false,
      canWithdraw: false,
      canCancel: false,
      canHide: false,
    }
    answer({ '/api/v1/private/requests': [request] })
    area('/private/requests?request=2')
    const link = await screen.findByRole('link', { name: 'Scene B' })
    await waitFor(() => expect(document.activeElement).toBe(link))
    expect(link.closest('li')?.hasAttribute('data-highlighted')).toBe(true)
  })
})

describe('private header search', () => {
  beforeEach(() => vi.useFakeTimers({ shouldAdvanceTime: true }))
  afterEach(() => vi.useRealTimers())

  function pill(path: string) {
    return renderWithProviders(
      <>
        <LocationProbe />
        <PrivateSearchPill />
        <Link to="/">Home</Link>
      </>,
      { path, me: UNLOCKED_MEMBER },
    )
  }

  it('searches live 250 ms after typing, with the text only in a /private URL (PRIV-12)', async () => {
    pill('/private')
    const box = screen.getByRole('searchbox', { name: 'Search Private' })
    expect(box.getAttribute('placeholder')).toMatch(/scenes, performers, studios/)
    fireEvent.change(box, { target: { value: 'Needle' } })
    act(() => vi.advanceTimersByTime(200))
    expect(location()).toBe('/private')
    act(() => vi.advanceTimersByTime(60))
    expect(location()).toBe('/private/search?q=Needle')
  })

  it('fills from the results URL and keeps nothing once the area is left', async () => {
    pill('/private/search?q=Needle')
    expect((screen.getByRole('searchbox') as HTMLInputElement).value).toBe('Needle')
    fireEvent.click(screen.getByRole('link', { name: 'Home' }))
    expect(location()).toBe('/')
  })
})

describe('private scene page (spec 0008 Q8 A, PRIV-05, PRIV-06)', () => {
  const DETAILS =
    'Placeholder line one.\n\nPlaceholder line two with <b>markup</b> that stays text.'
  const MORE: PrivateCard[] = [
    { ...CARD, id: SCENE_B, title: 'Scene B', durationSeconds: 2280 },
    { ...CARD, id: SCENE_C, title: 'Scene C', status: 'available' },
    // The scene itself never shows in its own "More from".
    CARD,
  ]
  const FULL: PrivateScene = { ...SCENE, details: DETAILS, moreFromStudio: MORE }

  /** `matchMedia` where only the desktop width (≥ 1024 px) matches, or nothing. */
  function viewport(desktop: boolean) {
    vi.stubGlobal(
      'matchMedia',
      vi.fn((query: string) => ({
        matches: desktop && query === '(min-width: 1024px)',
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      })),
    )
  }

  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    toast.dismiss()
    document.title = 'Starr'
  })
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('puts the request card in the side column on desktop, beside the image and description', async () => {
    viewport(true)
    answer({ '/api/v1/private/scenes/{sceneId}': FULL })
    area(`/private/scenes/${SCENE_A}`)
    await screen.findByRole('heading', { name: 'Scene A' })
    const side = screen.getByRole('complementary', { name: 'Request and more scenes' })
    expect(within(side).getByRole('region', { name: 'Request and status' })).toBeTruthy()
    expect(within(side).getByRole('list', { name: 'More from Studio S' })).toBeTruthy()
    // The description is in the main column, not the side.
    expect(within(side).queryByRole('region', { name: 'Description' })).toBeNull()
    expect(screen.getByRole('region', { name: 'Description' })).toBeTruthy()
  })

  it('puts the request card directly under the title on mobile', async () => {
    viewport(false)
    answer({ '/api/v1/private/scenes/{sceneId}': FULL })
    area(`/private/scenes/${SCENE_A}`)
    const title = await screen.findByRole('heading', { name: 'Scene A', level: 1 })
    const card = screen.getByRole('region', { name: 'Request and status' })
    // On mobile the side column only holds "More from" and is named after it.
    const side = screen.getByRole('complementary', { name: 'More from Studio S' })
    expect(side.contains(card)).toBe(false)
    // The next block after the title's own (title and facts).
    expect(title.parentElement?.nextElementSibling?.firstElementChild).toBe(card)
    expect(screen.getAllByRole('region', { name: 'Request and status' })).toHaveLength(1)
    // Its heading continues the page's outline under the h1.
    expect(within(card).getByRole('heading', { level: 2, name: 'Request and status' })).toBeTruthy()
  })

  it('has no empty side column on mobile when the studio has nothing else', async () => {
    viewport(false)
    answer({ '/api/v1/private/scenes/{sceneId}': { ...FULL, moreFromStudio: [CARD] } })
    area(`/private/scenes/${SCENE_A}`)
    await screen.findByRole('heading', { name: 'Scene A', level: 1 })
    expect(screen.queryByRole('complementary')).toBeNull()
  })

  it('keeps focus on the request card when it moves between the columns at 1024 px', async () => {
    let desktop = false
    const listeners = new Set<() => void>()
    vi.stubGlobal(
      'matchMedia',
      vi.fn((query: string) => ({
        get matches() {
          return desktop && query === '(min-width: 1024px)'
        },
        media: query,
        addEventListener: (_: string, fn: () => void) => listeners.add(fn),
        removeEventListener: (_: string, fn: () => void) => listeners.delete(fn),
      })),
    )
    answer({
      '/api/v1/private': { ...AREA, requestable: true },
      '/api/v1/private/scenes/{sceneId}': { ...FULL, requestable: true },
    })
    area(`/private/scenes/${SCENE_A}`)
    const button = await screen.findByRole('button', { name: 'Request scene' })
    act(() => button.focus())
    act(() => {
      desktop = true
      for (const fn of listeners) fn()
    })
    const side = screen.getByRole('complementary', { name: 'Request and more scenes' })
    const card = within(side).getByRole('region', { name: 'Request and status' })
    expect(card.contains(document.activeElement)).toBe(true)
  })

  it('says neutrally when a request or a withdraw fails', async () => {
    const waiting: PrivateScene = {
      ...FULL,
      status: 'requested',
      requestedByMe: true,
      requestable: true,
      journey: { steps: [{ step: 'awaiting_approval', status: 'current' }] },
    }
    answer({
      '/api/v1/private': { ...AREA, requestable: true },
      '/api/v1/private/scenes/{sceneId}': { ...FULL, requestable: true },
    })
    api.POST.mockResolvedValue(fail('catalog.unavailable', 503))
    const first = area(`/private/scenes/${SCENE_A}`)
    fireEvent.click(await screen.findByRole('button', { name: 'Request scene' }))
    await waitFor(() => expect(toast.getToasts().length).toBeGreaterThan(0))
    const leaked = /Scene A|Placeholder line|Studio S|Performer P/
    expect(String(toast.getToasts().at(-1)?.id)).toMatch(/^private:/)
    expect(JSON.stringify(toast.getToasts())).not.toMatch(leaked)
    first.unmount()
    toast.dismiss()

    answer({
      '/api/v1/private/scenes/{sceneId}': waiting,
      '/api/v1/private/requests': [
        {
          id: 4,
          kind: 'scene',
          category: 'adult',
          title: 'Scene A',
          titleId: SCENE_A,
          state: 'pending_approval',
          items: [{ state: 'pending_approval', journey: { steps: [] } }],
          openIn: [],
          createdAt: '2026-11-05T10:00:00Z',
          updatedAt: '2026-11-05T10:00:00Z',
          follow: false,
          hidden: false,
          canWithdraw: true,
          canCancel: false,
          canHide: false,
        },
      ],
    })
    api.POST.mockResolvedValue(fail('request.conflict', 409))
    area(`/private/scenes/${SCENE_A}`)
    fireEvent.click(await screen.findByRole('button', { name: 'Withdraw request' }))
    await waitFor(() => expect(toast.getToasts().length).toBeGreaterThan(0))
    expect(String(toast.getToasts().at(-1)?.id)).toMatch(/^private:/)
    expect(JSON.stringify(toast.getToasts())).not.toMatch(leaked)
  })

  it('shows the description as plain text, three lines until "Show more"', async () => {
    // jsdom lays nothing out: say the clamped text is taller than its box.
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockReturnValue(200)
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(72)
    answer({ '/api/v1/private/scenes/{sceneId}': FULL })
    area(`/private/scenes/${SCENE_A}`)
    const box = await screen.findByRole('region', { name: 'Description' })
    expect(within(box).getByText(/that stays text/).textContent).toBe(
      'Placeholder line two with <b>markup</b> that stays text.',
    )
    expect(box.querySelector('b')).toBeNull()
    const more = within(box).getByRole('button', { name: 'Show more' })
    const body = document.getElementById(more.getAttribute('aria-controls') ?? '')
    expect(more.getAttribute('aria-expanded')).toBe('false')
    expect(body?.className).toContain('line-clamp-3')
    fireEvent.click(more)
    const less = within(box).getByRole('button', { name: 'Show less' })
    expect(less.getAttribute('aria-expanded')).toBe('true')
    expect(body?.className).not.toContain('line-clamp-3')
  })

  it('has no "Show more" when the description fits, and no box without one', async () => {
    answer({ '/api/v1/private/scenes/{sceneId}': FULL })
    const first = area(`/private/scenes/${SCENE_A}`)
    const box = await screen.findByRole('region', { name: 'Description' })
    expect(within(box).queryByRole('button')).toBeNull()
    first.unmount()
    answer({ '/api/v1/private/scenes/{sceneId}': { ...FULL, details: null } })
    area(`/private/scenes/${SCENE_A}`)
    await screen.findByRole('heading', { name: 'Scene A' })
    expect(screen.queryByRole('region', { name: 'Description' })).toBeNull()
  })

  it('never puts the title or description into the browser title, the URL, a toast or a live region', async () => {
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(
        ok(
          path === '/api/v1/private'
            ? { ...AREA, requestable: true }
            : path === '/api/v1/private/scenes/{sceneId}'
              ? { ...FULL, requestable: true }
              : [],
        ),
      ),
    )
    api.POST.mockResolvedValue(
      ok({ requests: [{ id: 9, state: 'pending_approval' }], skipped: [] }, 201),
    )
    area(`/private/scenes/${SCENE_A}`)
    await screen.findByRole('heading', { name: 'Scene A' })
    fireEvent.click(await screen.findByRole('button', { name: 'Request scene' }))
    await waitFor(() => expect(toast.getToasts().length).toBeGreaterThan(0))
    expectNeutralPrivateToast('Scene requested – waiting for approval')
    const leaked = /Scene A|Placeholder line|Studio S|Performer P/
    expect(document.title).not.toMatch(leaked)
    expect(location()).toBe(`/private/scenes/${SCENE_A}`)
    expect(JSON.stringify(toast.getToasts())).not.toMatch(leaked)
    for (const live of Array.from(document.querySelectorAll('[aria-live]'))) {
      expect(live.textContent).not.toMatch(leaked)
    }
  })

  it('is the same "not found" as any unknown address when the gate closes on the server', async () => {
    const unknown = area('/nope')
    const expected = page(unknown.container)
    unknown.unmount()
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(path === '/api/v1/private' ? ok(AREA) : fail('private.not_found', 404)),
    )
    const view = area(`/private/scenes/${SCENE_A}`)
    expect(await screen.findByText('Nothing out here')).toBeTruthy()
    expect(view.container.innerHTML).not.toMatch(/Placeholder line|Scene A|data-ambient/)
    expect(view.container.querySelector('img')).toBeNull()
    // Inside the area's frame (its sub-navigation) the very "not found" of any address.
    expect(page(view.container)).toContain(expected)
  })

  it('lists "More from <studio>" without the scene itself, one tab stop, and links the studio', async () => {
    answer({ '/api/v1/private/scenes/{sceneId}': FULL })
    area(`/private/scenes/${SCENE_A}`)
    const list = await screen.findByRole('list', { name: 'More from Studio S' })
    const links = within(list).getAllByRole('link')
    expect(links.map((l) => l.getAttribute('aria-label'))).toEqual([
      'Scene B, Performer P, Oct 30, 2026, 38 min',
      'Scene C, Performer P, Oct 30, 2026, 30 min, Available',
    ])
    expect(links.map((l) => l.getAttribute('href'))).toEqual([
      `/private/scenes/${SCENE_B}`,
      `/private/scenes/${SCENE_C}`,
    ])
    expect(links.map((l) => l.tabIndex)).toEqual([0, -1])
    act(() => links[0]?.focus())
    fireEvent.keyDown(links[0] as HTMLElement, { key: 'ArrowRight' })
    expect(document.activeElement).toBe(links[1])
    expect(screen.getByRole('link', { name: 'All scenes' }).getAttribute('href')).toBe(
      `/private/studios/${STUDIO}`,
    )
  })

  it('shows the performers round and the studio as a logo tile from its page (A6)', async () => {
    answer({
      '/api/v1/private/scenes/{sceneId}': FULL,
      '/api/v1/private/studios/{studioId}': {
        kind: 'studio',
        id: STUDIO,
        name: 'Studio S',
        image: IMAGE,
        scenes: [],
        requestable: false,
      },
    })
    const { container } = area(`/private/scenes/${SCENE_A}`)
    await screen.findByRole('heading', { name: 'Scene A' })
    const performer = screen.getByRole('link', { name: 'Performer P' })
    expect(performer.querySelector('.rounded-full.aspect-square')?.textContent).toBe('P')
    await waitFor(() =>
      expect(
        container.querySelector(`img[src="/api/v1/images/stashdb/600/${IMAGE}"].object-contain`),
      ).toBeTruthy(),
    )
  })

  it('withdraws a waiting request from the card with a neutral toast (MEM-05)', async () => {
    const waiting: PrivateScene = {
      ...FULL,
      status: 'requested',
      requestedByMe: true,
      requestable: true,
      journey: {
        steps: [
          { step: 'requested', status: 'done' },
          { step: 'awaiting_approval', status: 'current', note: 'can_withdraw' },
        ],
      },
    }
    const mine = {
      id: 4,
      kind: 'scene',
      category: 'adult',
      title: 'Scene A',
      titleId: SCENE_A,
      state: 'pending_approval',
      items: [{ state: 'pending_approval', journey: { steps: [] } }],
      openIn: [],
      createdAt: '2026-11-05T10:00:00Z',
      updatedAt: '2026-11-05T10:00:00Z',
      follow: false,
      hidden: false,
      canWithdraw: true,
      canCancel: false,
      canHide: false,
    }
    answer({
      '/api/v1/private/scenes/{sceneId}': waiting,
      '/api/v1/private/requests': [mine],
    })
    api.POST.mockResolvedValue(ok({}))
    area(`/private/scenes/${SCENE_A}`)
    const card = await screen.findByRole('region', { name: 'Request and status' })
    fireEvent.click(await within(card).findByRole('button', { name: 'Withdraw request' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests/{id}/withdraw', {
        params: { path: { id: 4 } },
      }),
    )
    await waitFor(() => expectNeutralPrivateToast('Scene request withdrawn'))
  })

  // The entrance itself is CSS inside `prefers-reduced-motion: no-preference` (index.css, as
  // the overlay's): with reduced motion these classes animate nothing.
  it('enters like the detail overlay over the ambient layer of its own image', async () => {
    answer({ '/api/v1/private/scenes/{sceneId}': FULL })
    const { container } = area(`/private/scenes/${SCENE_A}`)
    await screen.findByRole('heading', { name: 'Scene A' })
    expect(container.querySelectorAll('.detail-settle')).toHaveLength(1)
    expect(container.querySelector('.detail-stagger h1')).toBeTruthy()
    expect(container.querySelector('.scene-side')).toBeTruthy()
    const ambient = container.querySelector('[data-ambient]')
    expect(ambient?.getAttribute('aria-hidden')).toBe('true')
    expect(ambient?.querySelector('img')?.getAttribute('src')).toBe(
      `/api/v1/images/stashdb/300/${IMAGE}`,
    )
  })
})
