import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { DiscoverCategory, Highlight } from '@/api/discover'
import { mockReducedMotion } from '@/test/motion'
import { ok, renderWithProviders } from '@/test/render'

import { Hero } from './Hero'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))
const toast = vi.hoisted(() => Object.assign(vi.fn(), { error: vi.fn() }))
vi.mock('sonner', () => ({ toast }))

function highlight(id: string, title: string, extra: Partial<Highlight> = {}): Highlight {
  return {
    kind: 'movie',
    id,
    title,
    year: 2024,
    poster: `${id}-p.jpg`,
    backdrop: `${id}-b.jpg`,
    overview: `About ${title}.`,
    genres: ['Drama', 'Mystery'],
    rating: 7.84,
    status: 'new',
    requestedByMe: false,
    journeys: [],
    category: 'movies',
    rank: 1,
    ...extra,
  }
}

const ITEMS: Highlight[] = [
  highlight('1', 'Glass Harbor'),
  highlight('2', 'Night Ferry', { kind: 'series', category: 'series', rank: 1 }),
  highlight('3', 'Paper Moons', { rank: 2, status: 'available' }),
]

const CATEGORIES: DiscoverCategory[] = ['movies', 'series'].map((category) => ({
  category: category as DiscoverCategory['category'],
  kind: category === 'movies' ? 'movie' : 'series',
  discoverReady: true,
  requestable: true,
  lists: ['trending'],
  providers: [],
  genres: [],
  attributions: [],
}))

function serve(items: Highlight[] = ITEMS) {
  api.GET.mockImplementation((path: string, init?: { params?: { path?: { id?: string } } }) => {
    if (path === '/api/v1/discover/highlights') return Promise.resolve(ok(items))
    if (path === '/api/v1/discover') return Promise.resolve(ok(CATEGORIES))
    const id = init?.params?.path?.id
    return Promise.resolve(
      ok({
        id,
        runtimeMinutes: 128,
        seasonCount: 3,
        certification: { region: 'CH', rating: '12' },
        openIn:
          id === '3' || id === '4'
            ? [{ name: 'Wohnzimmer', service: 'jellyfin', url: 'https://jf/3' }]
            : [],
      }),
    )
  })
}

const slides = () => screen.getAllByRole('group', { hidden: true })
const current = () => slides().find((s) => !s.hasAttribute('inert')) as HTMLElement
/** The dwell's CSS animation ran out (React listens to the prefixed name where jsdom has no
 * `AnimationEvent`, to the plain one in browsers; only one of them reaches the handler). */
function endDwell(el: HTMLElement) {
  for (const name of ['animationend', 'webkitAnimationEnd']) {
    fireEvent(el, new Event(name, { bubbles: true }))
  }
}
const live = (container: HTMLElement) => container.querySelector<HTMLElement>('p[aria-live]')
const progress = (container: HTMLElement) =>
  container.querySelector<HTMLElement>('[data-hero-progress]')

describe('Hero "Bühne" (spec 0008 A4, A5)', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    toast.mockReset()
    mockReducedMotion(false)
  })
  afterEach(() => vi.unstubAllGlobals())

  it('is a labelled carousel of slides; only the current one is reachable', async () => {
    serve()
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    const region = await screen.findByRole('region', { name: 'Top 10 this week' })
    expect(region.getAttribute('aria-roledescription')).toBe('carousel')
    expect(slides()).toHaveLength(3)
    expect(slides()[0]?.getAttribute('aria-roledescription')).toBe('slide')
    expect(slides()[0]?.getAttribute('aria-label')).toBe('1 of 3: Glass Harbor')
    expect(slides().filter((s) => !s.hasAttribute('inert'))).toEqual([slides()[0]])
    // F-2: TMDB's trend, not Starr's own.
    expect(within(current()).getByText('#1')).toBeTruthy()
    expect(within(current()).getByText('Trending this week')).toBeTruthy()
    expect(within(current()).getByText('Movie')).toBeTruthy()
    expect(within(current()).getByRole('heading', { name: 'Glass Harbor' })).toBeTruthy()
    // Facts: rating and year from the card, runtime and certification once details are in.
    expect(within(current()).getByText('7.8')).toBeTruthy()
    expect(await within(current()).findByText('2 h 8 min')).toBeTruthy()
    expect(within(current()).getByText('Rated 12')).toBeTruthy()
    const tabs = within(screen.getByRole('tablist', { name: 'Choose title' })).getAllByRole('tab')
    expect(tabs.map((t) => t.getAttribute('aria-selected'))).toEqual(['true', 'false', 'false'])
    expect(tabs[0]?.getAttribute('aria-current')).toBe('true')
    // The thumbnail list is one tab stop.
    expect(tabs.map((t) => t.tabIndex)).toEqual([0, -1, -1])
  })

  it('advances when the dwell ends, and the ambient layer follows the slide', async () => {
    serve()
    const onActive = vi.fn()
    const { container } = renderWithProviders(<Hero onOpen={vi.fn()} onActiveChange={onActive} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    expect(onActive).toHaveBeenLastCalledWith(expect.objectContaining({ id: '1' }))
    const bar = progress(container)
    expect(bar?.style.animationPlayState).toBe('running')
    endDwell(bar as HTMLElement)
    expect(current().getAttribute('aria-label')).toBe('2 of 3: Night Ferry')
    expect(onActive).toHaveBeenLastCalledWith(expect.objectContaining({ id: '2' }))
    // Rotating by itself: not announced.
    expect(live(container)?.getAttribute('aria-live')).toBe('off')
  })

  it('pauses on hover and on focus, and with the pause button (WCAG 2.2.2)', async () => {
    serve()
    const { container } = renderWithProviders(<Hero onOpen={vi.fn()} />)
    const region = await screen.findByRole('region', { name: 'Top 10 this week' })
    fireEvent.pointerEnter(region, { pointerType: 'mouse' })
    expect(progress(container)?.style.animationPlayState).toBe('paused')
    fireEvent.pointerLeave(region, { pointerType: 'mouse' })
    expect(progress(container)?.style.animationPlayState).toBe('running')

    const pause = screen.getByRole('button', { name: 'Pause carousel' })
    fireEvent.click(pause)
    expect(screen.getByRole('button', { name: 'Play carousel' })).toBe(pause)
    fireEvent.blur(pause, { relatedTarget: document.body })
    expect(progress(container)?.style.animationPlayState).toBe('paused')
    // Stopped: the slide on show is announced as one sentence.
    expect(live(container)?.getAttribute('aria-live')).toBe('polite')
    expect(live(container)?.textContent).toBe('1 of 3: Glass Harbor')
  })

  it('moves with thumbnails, previous/next and the arrow keys', async () => {
    serve()
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    const tabs = () => screen.getAllByRole('tab')
    fireEvent.click(tabs()[2] as HTMLElement)
    expect(current().getAttribute('aria-label')).toBe('3 of 3: Paper Moons')
    fireEvent.click(screen.getByRole('button', { name: 'Next title' }))
    expect(current().getAttribute('aria-label')).toBe('1 of 3: Glass Harbor')
    fireEvent.click(screen.getByRole('button', { name: 'Previous title' }))
    expect(current().getAttribute('aria-label')).toBe('3 of 3: Paper Moons')
    // Arrow keys on the thumbnails move the slide and the focus with it.
    const third = tabs()[2] as HTMLElement
    third.focus()
    fireEvent.keyDown(third, { key: 'ArrowRight' })
    expect(current().getAttribute('aria-label')).toBe('1 of 3: Glass Harbor')
    await waitFor(() => expect(document.activeElement).toBe(tabs()[0]))
  })

  it('keeps focus in the carousel when the arrow keys move away from its actions', async () => {
    serve()
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    const details = within(current()).getByRole('button', { name: 'Details' })
    details.focus()
    fireEvent.keyDown(details, { key: 'ArrowRight' })
    expect(current().getAttribute('aria-label')).toBe('2 of 3: Night Ferry')
    await waitFor(() => expect(current().contains(document.activeElement)).toBe(true))
  })

  it('requests a movie in place and keeps focus off <body>', async () => {
    serve()
    api.POST.mockResolvedValue(
      ok({ request: { state: 'pending_approval', title: 'Glass Harbor', items: [] }, skipped: [] }),
    )
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    const request = await within(current()).findByRole('button', { name: 'Request' })
    // One gold action on the slide.
    expect(request.className).toContain('bg-polaris')
    request.focus()
    fireEvent.click(request)
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests', {
        body: { kind: 'movie', titleId: '1' },
      }),
    )
    expect(await within(current()).findByText('Waiting for approval')).toBeTruthy()
    expect(toast).toHaveBeenCalledWith(
      "Requested “Glass Harbor”. It waits for your admin's approval.",
    )
    expect(document.activeElement).toBe(within(current()).getByRole('button', { name: 'Details' }))
  })

  it('lets a series choose its seasons in the detail view, and opens details', async () => {
    serve()
    const onOpen = vi.fn()
    renderWithProviders(<Hero onOpen={onOpen} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    fireEvent.click(screen.getAllByRole('tab')[1] as HTMLElement)
    fireEvent.click(await within(current()).findByRole('button', { name: 'Choose seasons' }))
    expect(onOpen).toHaveBeenLastCalledWith(expect.objectContaining({ id: '2', kind: 'series' }))
    expect(await within(current()).findByText('3 seasons')).toBeTruthy()
    fireEvent.click(within(current()).getByRole('button', { name: 'Details' }))
    expect(onOpen).toHaveBeenCalledTimes(2)
  })

  it('offers "Open in" in gold beside the status plaque of an available title', async () => {
    serve()
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    fireEvent.click(screen.getAllByRole('tab')[2] as HTMLElement)
    const plaque = current().querySelector<HTMLElement>('[data-status-plaque]') as HTMLElement
    expect(within(plaque).getByText('Available')).toBeTruthy()
    // The detail line names the server once the details say where it opens.
    expect(await within(plaque).findByText('in Wohnzimmer')).toBeTruthy()
    const link = within(current()).getByRole('link', { name: /Open in Wohnzimmer/ })
    expect(link.getAttribute('href')).toBe('https://jf/3')
    // Nothing to request: opening it is the slide's one gold action (A4, amended 2026-11-07).
    expect(link.className).toContain('bg-polaris')
    expect(within(current()).queryByRole('button', { name: 'Request' })).toBeNull()
    expect(within(current()).getByRole('button', { name: 'Details' }).className).not.toContain(
      'bg-polaris',
    )
  })

  it('shows a download as a ring with its share, and when a request was made', async () => {
    serve([
      highlight('1', 'Glass Harbor', {
        status: 'requested',
        requestedByMe: true,
        journeys: [
          {
            journey: {
              steps: [
                { step: 'requested', status: 'done', at: '2026-11-02T10:00:00Z' },
                { step: 'downloading', status: 'current', progress: 0.62 },
              ],
            },
          },
        ],
      }),
      highlight('2', 'Night Ferry', {
        status: 'requested',
        requestedByMe: true,
        journeys: [
          {
            journey: {
              steps: [
                { step: 'requested', status: 'done', at: '2026-11-02T10:00:00Z' },
                { step: 'awaiting_approval', status: 'current' },
              ],
            },
          },
        ],
      }),
      highlight('3', 'Paper Moons', { status: 'requested', requestedByMe: false }),
      highlight('4', 'Low Tide', {
        kind: 'series',
        category: 'series',
        status: 'partially_available',
      }),
    ])
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    const plaque = () => current().querySelector<HTMLElement>('[data-status-plaque]') as HTMLElement
    // Downloading: the ring carries the number, the lines say it in words.
    expect(within(plaque()).getByText('Downloading')).toBeTruthy()
    expect(within(plaque()).getByText('62% downloaded')).toBeTruthy()
    expect(plaque().querySelector('svg')).not.toBeNull()
    // Waiting: when it was requested.
    fireEvent.click(screen.getAllByRole('tab')[1] as HTMLElement)
    expect(within(plaque()).getByText('Waiting for approval')).toBeTruthy()
    expect(within(plaque()).getByText('requested Nov 2')).toBeTruthy()
    // Nothing more known: one line.
    fireEvent.click(screen.getAllByRole('tab')[2] as HTMLElement)
    expect(plaque().textContent).toBe('Requested by someone')
    // No gold where there is nothing to request or open.
    expect(current().querySelector('.bg-polaris')).toBeNull()
    // Partly available: the plaque, and opening it is the one gold action.
    fireEvent.click(screen.getAllByRole('tab')[3] as HTMLElement)
    expect(within(plaque()).getByText('Partly available')).toBeTruthy()
    expect(await within(plaque()).findByText('in Wohnzimmer')).toBeTruthy()
    const open = within(current()).getByRole('link', { name: /Open in Wohnzimmer/ })
    expect(open.className).toContain('bg-polaris')
    expect(current().querySelectorAll('.bg-polaris')).toHaveLength(1)
  })

  it('never moves by itself under reduced motion and says so', async () => {
    mockReducedMotion(true)
    serve()
    const { container } = renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    expect(progress(container)).toBeNull()
    expect(screen.getByText('Autoplay off')).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Pause carousel' })).toBeNull()
    // No cross-fade or zoom: nothing transitions.
    for (const slide of slides()) expect(slide.className).toContain('motion-reduce:transition-none')
  })

  it('loads only images through the proxy, the first slide eagerly', async () => {
    serve()
    const { container } = renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    const imgs = Array.from(container.querySelectorAll('img'))
    expect(imgs.length).toBeGreaterThan(0)
    for (const img of imgs)
      expect(img.getAttribute('src')?.startsWith('/api/v1/images/')).toBe(true)
    const first = slides()[0]?.querySelector('img')
    expect(first?.getAttribute('loading')).toBe('eager')
    expect(first?.getAttribute('src')).toBe('/api/v1/images/tmdb/w780/1-b.jpg')
  })

  it('says when the top 10 cannot load, with a retry', async () => {
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(
        path === '/api/v1/discover/highlights'
          ? {
              data: undefined,
              error: { code: 'unavailable' },
              response: new Response(null, { status: 503 }),
            }
          : ok(CATEGORIES),
      ),
    )
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    expect(await screen.findByText("The top 10 couldn't load.")).toBeTruthy()
    serve()
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
    expect(await screen.findByRole('region', { name: 'Top 10 this week' })).toBeTruthy()
  })

  it('keeps Request and its focus when requesting fails, and says why', async () => {
    serve()
    api.POST.mockResolvedValue({
      data: undefined,
      error: { code: 'unavailable' },
      response: new Response(null, { status: 503 }),
    })
    renderWithProviders(<Hero onOpen={vi.fn()} />)
    await screen.findByRole('region', { name: 'Top 10 this week' })
    const request = await within(current()).findByRole('button', { name: 'Request' })
    request.focus()
    fireEvent.click(request)
    await waitFor(() => expect(toast.error).toHaveBeenCalled())
    expect(within(current()).getByRole('button', { name: 'Request' })).toBe(request)
    expect(document.activeElement).toBe(request)
  })

  it('takes no room without highlights', async () => {
    serve([])
    const { container } = renderWithProviders(<Hero onOpen={vi.fn()} />)
    await act(async () => {})
    await waitFor(() => expect(container.querySelector('[aria-busy]')).toBeNull())
    expect(screen.queryByRole('region')).toBeNull()
  })
})
