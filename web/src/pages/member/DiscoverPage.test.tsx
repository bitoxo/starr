import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { useLocation } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { DiscoverCategory, TitlePage } from '@/api/discover'
import {
  BROWSE_DELAY,
  BROWSE_WINDOW,
  LEAVE_GRACE,
  PREVIEW_DELAY,
  resetPreviewBrowsing,
} from '@/lib/preview'
import { DetailViewHost } from '@/components/DetailView'
import { mockReducedMotion } from '@/test/motion'
import { ok, renderWithProviders } from '@/test/render'

import DiscoverPage from './DiscoverPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))
vi.mock('sonner', () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }))

const MOVIES: DiscoverCategory = {
  category: 'movies',
  kind: 'movie',
  discoverReady: true,
  requestable: true,
  lists: ['trending', 'popular', 'top_rated'],
  providers: [],
  genres: [],
  attributions: [],
}

const PAGE: TitlePage = {
  items: [
    {
      kind: 'movie',
      id: '27205',
      title: 'Inception',
      year: 2010,
      poster: 'oYuLEt3zVCKq57qu2F8dT7NIa6f.jpg',
      backdrop: 'inception-b.jpg',
      overview: 'A thief who steals secrets through dreams.',
      rating: 8.4,
      status: 'available',
      requestedByMe: false,
      journeys: [],
      genres: ['Action', 'Science Fiction'],
    },
    {
      kind: 'movie',
      id: '496243',
      title: 'Parasite',
      year: 2019,
      poster: null,
      status: 'new',
      requestedByMe: false,
      journeys: [],
      genres: [],
    },
  ],
  next: 2,
}

/** A title's details, as far as the preview card and the strip read them ("Open in"). */
const DETAILS = {
  openIn: [{ name: 'Jellyfin', service: 'jellyfin', url: 'https://jf.example/item/1' }],
}

function serve(categories: DiscoverCategory[], page: TitlePage = PAGE) {
  api.GET.mockImplementation((path: string) =>
    Promise.resolve(
      path === '/api/v1/discover'
        ? ok(categories)
        : path === '/api/v1/discover/highlights'
          ? ok([])
          : path === '/api/v1/titles/{kind}/{id}'
            ? ok(DETAILS)
            : ok(page),
    ),
  )
}

/** Records Web Animations calls (jsdom has none): which properties each animation moves. */
function recordAnimations() {
  const calls: {
    el: Element
    keyframes: Keyframe[]
    options: KeyframeAnimationOptions
    animation: { onfinish: (() => void) | null }
  }[] = []
  Element.prototype.animate = vi.fn(function (
    this: Element,
    keyframes: Keyframe[],
    options: KeyframeAnimationOptions,
  ) {
    const animation = { cancel: vi.fn(), onfinish: null, playState: 'finished' }
    calls.push({ el: this, keyframes, options, animation })
    return animation as unknown as Animation
  }) as unknown as typeof Element.prototype.animate
  return calls
}

function stopRecording() {
  // @ts-expect-error -- jsdom has no `animate`; remove the test double again
  delete Element.prototype.animate
}

/** The cover inside a lane tile, which the card's poster stands in for. */
const coverIn = (tile: HTMLElement) => tile.querySelector('button > span') as HTMLElement

/** The current URL's query, to check what survives a reload (spec 0008 F6). */
function Search() {
  return <output data-testid="search">{useLocation().search}</output>
}

/** Focus as the keyboard does it (`:focus-visible`; a click before would make it a pointer's). */
function keyboardFocus(el: HTMLElement) {
  fireEvent.keyDown(document.body, { key: 'Tab' })
  act(() => el.focus())
}

function stubHover(canHover: boolean) {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: query === '(hover: none)' ? !canHover : false,
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  )
}

describe('Discover', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    resetPreviewBrowsing()
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    stopRecording()
    vi.restoreAllMocks()
  })

  it('explains when no category is open for the member', async () => {
    serve([])
    renderWithProviders(<DiscoverPage />)
    expect(await screen.findByText('The sky is still dark')).toBeTruthy()
  })

  it('explains when Discover is not set up yet', async () => {
    serve([{ ...MOVIES, discoverReady: false, requestable: false, lists: [] }])
    renderWithProviders(<DiscoverPage />)
    expect(await screen.findByText('Discover is still being set up')).toBeTruthy()
  })

  it('reserves the hero and the lanes while it loads', () => {
    api.GET.mockReturnValue(new Promise(() => {}))
    const { container } = renderWithProviders(<DiscoverPage />)
    expect(container.querySelector('[data-hero-skeleton]')).not.toBeNull()
    expect(screen.getByRole('heading', { level: 1, name: 'Discover' })).toBeTruthy()
  })

  it('shows covers without captions, the state on the cover (spec 0008 A3)', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const lane = (await screen.findByRole('list', { name: 'Movies: Trending' })) as HTMLElement
    const inception = await within(lane).findByRole('button', {
      name: 'Inception (2010), Available',
    })
    // No title or year under the cover; the name and the chip carry them.
    expect(within(lane).queryByText('Inception')).toBeNull()
    expect(within(lane).queryByText('2010')).toBeNull()
    expect(within(lane).getByText('Available')).toBeTruthy()
    // New titles stay bare in lanes.
    expect(within(lane).queryByText('New')).toBeNull()
    expect(inception).toBeTruthy()
  })

  it('links each lane to the "See all" grid of its chosen list (spec 0008 Q6 B)', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />, { path: '/?lane-movies=top_rated' })
    const link = await screen.findByRole('link', { name: 'See all: Movies: Top rated' })
    expect(link.getAttribute('href')).toBe('/discover/movies/top_rated')
  })

  it('keeps the chosen list in the URL and the lists as one tab stop (MEM-12)', async () => {
    serve([MOVIES])
    renderWithProviders(
      <>
        <DiscoverPage />
        <Search />
      </>,
      { path: '/?lane-movies=popular' },
    )
    const tabs = await screen.findByRole('tablist', { name: 'Movies lists' })
    const tab = (name: string) => within(tabs).getByRole('tab', { name })
    expect(tab('Popular').getAttribute('aria-selected')).toBe('true')
    expect(
      within(tabs)
        .getAllByRole('tab')
        .map((t) => t.tabIndex),
    ).toEqual([-1, 0, -1])
    await waitFor(() =>
      expect(api.GET).toHaveBeenCalledWith('/api/v1/discover/{category}/{list}', {
        params: { path: { category: 'movies', list: 'popular' }, query: { page: 1 } },
      }),
    )
    tab('Popular').focus()
    fireEvent.keyDown(tab('Popular'), { key: 'ArrowRight' })
    expect(tab('Top rated').getAttribute('aria-selected')).toBe('true')
    expect(document.activeElement).toBe(tab('Top rated'))
    expect(screen.getByTestId('search').textContent).toBe('?lane-movies=top_rated')
    // The default list is not written to the URL.
    fireEvent.click(tab('Trending'))
    expect(screen.getByTestId('search').textContent).toBe('')
  })

  it('marks the edge of an overflowing list row with a chevron that scrolls (MEM-16)', async () => {
    const sizes = { scrollWidth: 420, clientWidth: 240 }
    for (const [key, value] of Object.entries(sizes)) {
      vi.spyOn(HTMLElement.prototype, key as keyof typeof sizes, 'get').mockImplementation(
        function (this: HTMLElement) {
          return this.getAttribute('role') === 'tablist' ? value : 0
        },
      )
    }
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const tabs = await screen.findByRole('tablist', { name: 'Movies lists' })
    const hint = (edge: string) => document.querySelector(`[data-scroll-hint="${edge}"]`)
    await waitFor(() => expect(hint('forward')).not.toBeNull())
    expect(hint('back')).toBeNull()
    // Not a tab stop: the arrow keys reach every list.
    expect((hint('forward') as HTMLElement).tabIndex).toBe(-1)
    tabs.scrollLeft = 180
    fireEvent.scroll(tabs)
    expect(hint('back')).not.toBeNull()
    expect(hint('forward')).toBeNull()
    vi.restoreAllMocks()
  })

  it('falls back to the first list for an unknown one in the URL', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />, { path: '/?lane-movies=nonsense' })
    const tabs = await screen.findByRole('tablist', { name: 'Movies lists' })
    expect(within(tabs).getByRole('tab', { name: 'Trending' }).getAttribute('aria-selected')).toBe(
      'true',
    )
  })

  it('makes a lane one tab stop; the info strip follows keyboard focus', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const lane = (await screen.findByRole('list', { name: 'Movies: Trending' })) as HTMLElement
    const inception = await within(lane).findByRole('button', {
      name: 'Inception (2010), Available',
    })
    const parasite = within(lane).getByRole('button', { name: 'Parasite (2019)' })
    // MEM-04: one stop per lane, the chip is no control of its own.
    const stops = within(lane)
      .getAllByRole('button')
      .filter((b) => b.tabIndex === 0)
    expect(stops).toEqual([inception])
    // Collapsed for pointer users without keyboard focus.
    expect(screen.queryByRole('group', { name: /^About/ })).toBeNull()
    keyboardFocus(inception)
    const strip = screen.getByRole('group', { name: 'About Inception' })
    expect(within(strip).getByRole('heading', { name: 'Inception' })).toBeTruthy()
    expect(within(strip).getByText('A thief who steals secrets through dreams.')).toBeTruthy()
    expect(within(strip).getByText('8.4')).toBeTruthy()
    expect(within(strip).getByText('Action, Science Fiction')).toBeTruthy()
    expect(within(strip).getByText('Available')).toBeTruthy()
    fireEvent.keyDown(inception, { key: 'ArrowRight' })
    expect(document.activeElement).toBe(parasite)
    const next = screen.getByRole('group', { name: 'About Parasite' })
    // Not requested yet: Request, the strip's one gold action, then Details.
    const request = within(next).getByRole('button', { name: 'Request' })
    expect(request.className).toContain('bg-polaris')
    expect(within(next).getByRole('button', { name: 'Details of Parasite' })).toBeTruthy()
    fireEvent.keyDown(parasite, { key: 'Home' })
    expect(document.activeElement).toBe(inception)
    // Ghost buttons for pointers stay out of the tab order.
    const region = screen.getByRole('region', { name: 'Movies' })
    for (const name of ['Scroll Movies: Trending back', 'Scroll Movies: Trending forward']) {
      expect(within(region).getByRole('button', { name }).tabIndex).toBe(-1)
    }
    // MEM-14: room above the covers for the lifted cover's ring inside the scroll box.
    expect(lane.className).toContain('pt-3')
    // Leaving the lane and its strip collapses the strip.
    fireEvent.blur(inception, { relatedTarget: document.body })
    expect(screen.queryByRole('group', { name: /^About/ })).toBeNull()
  })

  it('opens details from the strip via the cover, so focus can return there', async () => {
    serve([MOVIES])
    renderWithProviders(
      <>
        <DiscoverPage />
        <Search />
      </>,
    )
    const cover = await screen.findByRole('button', { name: 'Inception (2010), Available' })
    keyboardFocus(cover)
    const strip = screen.getByRole('group', { name: 'About Inception' })
    fireEvent.click(within(strip).getByRole('button', { name: 'Details of Inception' }))
    expect(document.activeElement).toBe(cover)
    expect(screen.getByTestId('search').textContent).toBe('?title=movie-27205')
  })

  it('focuses a clicked cover, so closing the detail view can return there', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'Inception (2010), Available' })
    fireEvent.click(cover)
    expect(document.activeElement).toBe(cover)
  })

  it('shows a preview card after hovering a cover with a mouse (pointer only)', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    const tile = cover.closest('li') as HTMLElement
    // Touch never shows it.
    fireEvent.pointerEnter(tile, { pointerType: 'touch' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY - 1))
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    act(() => vi.advanceTimersByTime(1))
    const card = document.querySelector<HTMLElement>('[data-preview-card]')
    expect(card).not.toBeNull()
    expect(card?.getAttribute('aria-hidden')).toBe('true')
    expect(within(card as HTMLElement).getAllByText('Parasite').length).toBeGreaterThan(0)
    // Nothing in it is a tab stop.
    for (const b of Array.from(card?.querySelectorAll('button') ?? [])) expect(b.tabIndex).toBe(-1)
    // Esc closes it.
    fireEvent.keyDown(document, { key: 'Escape' })
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    // So does scrolling the page under it.
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    fireEvent.scroll(document)
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    // So does leaving the cover.
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    fireEvent.pointerLeave(tile, { relatedTarget: document.body })
    // A short grace first, so the pointer can cross onto the card.
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    act(() => vi.advanceTimersByTime(LEAVE_GRACE))
    expect(document.querySelector('[data-preview-card]')).toBeNull()
  })

  it('opens the next card after 90 ms while browsing (intent timing)', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const inception = await screen.findByRole('button', { name: 'Inception (2010), Available' })
    const parasite = screen.getByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    const first = inception.closest('li') as HTMLElement
    const second = parasite.closest('li') as HTMLElement
    fireEvent.pointerEnter(first, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    expect(document.querySelectorAll('[data-preview-card]').length).toBe(1)
    // Moving on to the neighbour: its card after 90 ms, not after the full dwell.
    fireEvent.pointerLeave(first, { relatedTarget: second })
    fireEvent.pointerEnter(second, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(BROWSE_DELAY - 1))
    const before = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    expect(within(before).queryAllByText('Parasite')).toEqual([])
    act(() => vi.advanceTimersByTime(1))
    const cards = document.querySelectorAll<HTMLElement>('[data-preview-card]')
    expect(cards.length).toBe(1)
    expect(within(cards[0] as HTMLElement).getAllByText('Parasite').length).toBeGreaterThan(0)
  })

  it('opens the card on keyboard focus, with the focus ring on its poster', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const lane = (await screen.findByRole('list', { name: 'Movies: Trending' })) as HTMLElement
    const inception = await within(lane).findByRole('button', {
      name: 'Inception (2010), Available',
    })
    const parasite = within(lane).getByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    keyboardFocus(inception)
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    expect(card.getAttribute('aria-hidden')).toBe('true')
    expect(card.querySelector('.ring-polaris')).not.toBeNull()
    // The info strip keeps the actions for the keyboard.
    expect(screen.getByRole('group', { name: 'About Inception' })).toBeTruthy()
    // With a card open, moving focus (an arrow key in the browser) replaces it at once.
    keyboardFocus(parasite)
    const next = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    expect(within(next).getAllByText('Parasite').length).toBeGreaterThan(0)
    // Esc closes it; focus stays on the cover.
    fireEvent.keyDown(document, { key: 'Escape' })
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    expect(document.activeElement).toBe(parasite)
  })

  it('offers "Open in" as the gold action of an available title', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'Inception (2010), Available' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(cover.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    vi.useRealTimers()
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    const open = await within(card).findByRole('link', { name: 'Open in Jellyfin', hidden: true })
    expect(open.className).toContain('bg-polaris')
    expect(open.getAttribute('href')).toBe('https://jf.example/item/1')
    expect(open.tabIndex).toBe(-1)
    expect(within(card).getByText('Available in Jellyfin')).toBeTruthy()
    // Only an available title asks for its details.
    expect(api.GET).toHaveBeenCalledWith('/api/v1/titles/{kind}/{id}', {
      params: { path: { kind: 'movie', id: '27205' } },
    })
    expect(api.GET).not.toHaveBeenCalledWith('/api/v1/titles/{kind}/{id}', {
      params: { path: { kind: 'movie', id: '496243' } },
    })
    // The info strip offers the same for the keyboard.
    keyboardFocus(cover)
    const strip = screen.getByRole('group', { name: 'About Inception' })
    expect(within(strip).getByRole('link', { name: 'Open in Jellyfin' })).toBeTruthy()
  })

  it('shows a downloading title with its bar', async () => {
    serve([MOVIES], {
      items: [
        {
          kind: 'movie',
          id: '1',
          title: 'Arrival',
          year: 2016,
          status: 'requested',
          requestedByMe: true,
          genres: [],
          journeys: [
            {
              journey: {
                steps: [
                  { step: 'requested', status: 'done' },
                  { step: 'downloading', status: 'current', progress: 0.62 },
                ],
              },
            },
          ],
        },
      ],
      next: null,
    })
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: /^Arrival \(2016\), Downloading/ })
    vi.useFakeTimers()
    fireEvent.pointerEnter(cover.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    const bar = within(card).getByRole('progressbar', { hidden: true })
    expect(bar.getAttribute('aria-valuenow')).toBe('62')
    expect(within(card).queryByRole('button', { name: 'Request', hidden: true })).toBeNull()
  })

  it('morphs from the cover: clip-path area and flying poster, cover hidden meanwhile', async () => {
    const calls = recordAnimations()
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    const tile = cover.closest('li') as HTMLElement
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    const props = calls.flatMap((c) => c.keyframes.flatMap((k) => Object.keys(k)))
    expect(props).toContain('clipPath')
    expect(props).toContain('transform')
    const area = calls.find((c) => 'clipPath' in (c.keyframes[0] ?? {}))
    expect(area?.options.duration).toBe(320)
    // The card's poster stands in for the cover while it is open.
    expect(coverIn(tile).style.opacity).toBe('0')
    // Closing runs back at 0.7× (224 ms), then gives the cover back and goes.
    calls.length = 0
    fireEvent.keyDown(document, { key: 'Escape' })
    const back = calls.find((c) => 'clipPath' in (c.keyframes[0] ?? {}))
    expect(back?.options.duration).toBe(224)
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    act(() => back?.animation.onfinish?.())
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    expect(coverIn(tile).style.opacity).toBe('')
    // A card replaced while it is open gives its cover back too.
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    expect(coverIn(tile).style.opacity).toBe('0')
    const other = screen.getByRole('button', { name: 'Inception (2010), Available' })
    fireEvent.pointerLeave(tile, { relatedTarget: other })
    fireEvent.pointerEnter(other.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(BROWSE_DELAY))
    expect(coverIn(tile).style.opacity).toBe('')
    expect(coverIn(other.closest('li') as HTMLElement).style.opacity).toBe('0')
  })

  it('opens details from the preview card with focus on the cover', async () => {
    serve([MOVIES])
    renderWithProviders(
      <>
        <DiscoverPage />
        <Search />
      </>,
    )
    const cover = await screen.findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(cover.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    fireEvent.click(within(card).getByRole('button', { name: 'Details', hidden: true }))
    expect(document.activeElement).toBe(cover)
    expect(screen.getByTestId('search').textContent).toBe('?title=movie-496243')
    expect(document.querySelector('[data-preview-card]')).toBeNull()
  })

  it('opens details from the card’s poster and backdrop, not from its actions', async () => {
    serve([MOVIES])
    renderWithProviders(
      <>
        <DiscoverPage />
        <Search />
      </>,
    )
    const parasite = await screen.findByRole('button', { name: 'Parasite (2019)' })
    const inception = screen.getByRole('button', { name: 'Inception (2010), Available' })
    vi.useFakeTimers()
    const hover = (cover: HTMLElement) => {
      fireEvent.pointerEnter(cover.closest('li') as HTMLElement, { pointerType: 'mouse' })
      act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
      return document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    }
    // The poster: the detail overlay opens, focus on the cover for the way back.
    let card = hover(parasite)
    fireEvent.click(card.querySelector('[data-preview-poster]') as HTMLElement)
    expect(screen.getByTestId('search').textContent).toBe('?title=movie-496243')
    expect(document.activeElement).toBe(parasite)
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    // The backdrop: one target with the title, not a control of its own.
    act(() => vi.advanceTimersByTime(1000))
    card = hover(inception)
    const media = card.querySelector('.preview-media') as HTMLElement
    const title = within(card).getByText('Inception')
    expect(media.closest('button')).toBe(title.closest('button'))
    fireEvent.click(media)
    expect(screen.getByTestId('search').textContent).toBe('?title=movie-27205')
    expect(document.activeElement).toBe(inception)
  })

  it('returns focus to the cover when the overlay opened from the card’s poster closes', async () => {
    serve([MOVIES])
    renderWithProviders(
      <>
        <DiscoverPage />
        <DetailViewHost />
        <Search />
      </>,
    )
    const parasite = await screen.findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(parasite.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    vi.useRealTimers()
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    fireEvent.click(card.querySelector('[data-preview-poster]') as HTMLElement)
    const dialog = await screen.findByRole('dialog')
    fireEvent.keyDown(dialog, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    await waitFor(() => expect(document.activeElement).toBe(parasite))
  })

  it('leaves the card’s Request and "Open in" to themselves', async () => {
    serve([MOVIES])
    api.POST.mockResolvedValue(ok({ request: { state: 'submitted' } }))
    renderWithProviders(
      <>
        <DiscoverPage />
        <Search />
      </>,
    )
    const parasite = await screen.findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(parasite.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    vi.useRealTimers()
    let card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    fireEvent.click(within(card).getByRole('button', { name: 'Request', hidden: true }))
    await waitFor(() => expect(api.POST).toHaveBeenCalled())
    expect(screen.getByTestId('search').textContent).toBe('')
    fireEvent.keyDown(document, { key: 'Escape' })
    const inception = screen.getByRole('button', { name: 'Inception (2010), Available' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(inception.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    vi.useRealTimers()
    card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    const open = await within(card).findByRole('link', { name: 'Open in Jellyfin', hidden: true })
    fireEvent.click(open)
    expect(screen.getByTestId('search').textContent).toBe('')
  })

  it('keeps the card while the pointer is on it; browses quickly right after a close', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    let now = 1000
    vi.spyOn(performance, 'now').mockImplementation(() => now)
    const tile = cover.closest('li') as HTMLElement
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    // From the cover onto the card: it stays.
    fireEvent.pointerLeave(tile, { relatedTarget: card })
    act(() => vi.advanceTimersByTime(LEAVE_GRACE))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    fireEvent.pointerLeave(card, { relatedTarget: document.body })
    fireEvent.pointerEnter(card)
    act(() => vi.advanceTimersByTime(LEAVE_GRACE))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    fireEvent.pointerLeave(card, { relatedTarget: document.body })
    act(() => vi.advanceTimersByTime(LEAVE_GRACE))
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    // Back within 400 ms of the close: 90 ms.
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(BROWSE_DELAY))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    fireEvent.keyDown(document, { key: 'Escape' })
    // Later than that: the full dwell again.
    now += BROWSE_WINDOW
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(BROWSE_DELAY))
    expect(document.querySelector('[data-preview-card]')).toBeNull()
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY - BROWSE_DELAY))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
  })

  it('follows the cover while the lane scrolls after a key instead of closing', async () => {
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const lane = (await screen.findByRole('list', { name: 'Movies: Trending' })) as HTMLElement
    const cover = await within(lane).findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(cover.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    fireEvent.keyDown(cover, { key: 'ArrowLeft' })
    fireEvent.scroll(lane)
    act(() => vi.advanceTimersByTime(20))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    // A scroll of the user's own closes it at once.
    act(() => vi.advanceTimersByTime(1000))
    fireEvent.scroll(lane)
    expect(document.querySelector('[data-preview-card]')).toBeNull()
  })

  it('shows a waiting title with its chip, and no "Open in" where nothing is available', async () => {
    serve([MOVIES], {
      items: [
        {
          kind: 'movie',
          id: '2',
          title: 'Dune',
          year: 2021,
          status: 'requested',
          requestedByMe: true,
          genres: [],
          journeys: [{ journey: { steps: [{ step: 'awaiting_approval', status: 'current' }] } }],
        },
      ],
      next: null,
    })
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'Dune (2021), Waiting for approval' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(cover.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    expect(within(card).getByText('Waiting for approval')).toBeTruthy()
    expect(card.querySelector('.bg-polaris')).toBeNull()
    keyboardFocus(cover)
    const strip = screen.getByRole('group', { name: 'About Dune' })
    expect(within(strip).queryByRole('link', { name: /Open in/ })).toBeNull()
    expect(api.GET).not.toHaveBeenCalledWith('/api/v1/titles/{kind}/{id}', expect.anything())
  })

  it('makes an album card narrower, its square cover blurred in place of a backdrop', async () => {
    const album = '78570bea-2a26-467c-a3db-c52723ceb394'
    serve([{ ...MOVIES, category: 'music', kind: 'album', lists: ['trending'] }], {
      items: [
        {
          kind: 'album',
          id: album,
          title: 'After Hours',
          year: 2020,
          poster: album,
          artist: 'The Weeknd',
          status: 'new',
          requestedByMe: false,
          journeys: [],
          genres: [],
        },
      ],
      next: null,
    })
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'After Hours (2020), The Weeknd' })
    vi.useFakeTimers()
    fireEvent.pointerEnter(cover.closest('li') as HTMLElement, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    const card = document.querySelector<HTMLElement>('[data-preview-card]') as HTMLElement
    expect(card.style.width).toBe('340px')
    expect(card.querySelector('[data-square]')).not.toBeNull()
    expect(within(card).getByText('The Weeknd')).toBeTruthy()
  })

  it('only cross-fades the preview card under reduced motion (120 ms)', async () => {
    mockReducedMotion(true)
    const calls = recordAnimations()
    serve([MOVIES])
    renderWithProviders(<DiscoverPage />)
    const cover = await screen.findByRole('button', { name: 'Parasite (2019)' })
    vi.useFakeTimers()
    const tile = cover.closest('li') as HTMLElement
    fireEvent.pointerEnter(tile, { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(PREVIEW_DELAY))
    expect(document.querySelector('[data-preview-card]')).not.toBeNull()
    // Opacity only: nothing grows, flies or moves; the cover stays where it is.
    expect(calls.length).toBe(1)
    expect(calls[0]?.keyframes.flatMap((k) => Object.keys(k))).toEqual(['opacity', 'opacity'])
    expect(calls[0]?.options.duration).toBe(120)
    expect(coverIn(tile).style.opacity).toBe('')
  })

  it('on touch: a first tap selects and fills the strip, a second opens (F-5)', async () => {
    stubHover(false)
    serve([MOVIES])
    renderWithProviders(
      <>
        <DiscoverPage />
        <Search />
      </>,
    )
    const lane = (await screen.findByRole('list', { name: 'Movies: Trending' })) as HTMLElement
    const parasite = await within(lane).findByRole('button', { name: 'Parasite (2019)' })
    // No strip, and so no gold in the lane, before a cover is chosen.
    expect(screen.queryByRole('group', { name: /^About/ })).toBeNull()
    fireEvent.pointerDown(parasite, { pointerType: 'touch' })
    fireEvent.click(parasite)
    expect(screen.getByRole('group', { name: 'About Parasite' })).toBeTruthy()
    expect(screen.getByTestId('search').textContent).toBe('')
    fireEvent.pointerDown(parasite, { pointerType: 'touch' })
    fireEvent.click(parasite)
    expect(screen.getByTestId('search').textContent).toBe('?title=movie-496243')
  })

  it('shows music as square album covers through the proxy, with the artist', async () => {
    const album = '78570bea-2a26-467c-a3db-c52723ceb394'
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(
        path === '/api/v1/discover'
          ? ok([
              {
                category: 'music',
                kind: 'album',
                discoverReady: true,
                requestable: false,
                lists: ['trending', 'fresh_releases', 'popular'],
              },
            ])
          : path === '/api/v1/discover/highlights'
            ? ok([])
            : ok({
                items: [
                  {
                    kind: 'album',
                    id: album,
                    title: 'After Hours',
                    year: 2020,
                    poster: album,
                    artist: 'The Weeknd',
                    releaseType: 'album',
                    status: 'new',
                    requestedByMe: false,
                    journeys: [],
                    genres: [],
                  },
                ],
                next: null,
              }),
      ),
    )
    const { container } = renderWithProviders(<DiscoverPage />)
    const lane = (await screen.findByRole('region', { name: 'Music' })) as HTMLElement
    const cover = await within(lane).findByRole('button', {
      name: 'After Hours (2020), The Weeknd',
    })
    expect(within(lane).getByRole('tab', { name: 'Fresh releases' })).toBeTruthy()
    // No requests for music yet.
    expect(within(lane).getByText("Requests aren't open in this category yet.")).toBeTruthy()
    keyboardFocus(cover)
    const strip = screen.getByRole('group', { name: 'About After Hours' })
    expect(within(strip).getByText('The Weeknd')).toBeTruthy()
    expect(within(strip).queryByRole('button', { name: 'Request' })).toBeNull()
    const img = lane.querySelector('ul img')
    expect(img?.getAttribute('src')).toBe(`/api/v1/images/caa/500/${album}`)
    expect(img?.className).toContain('aspect-square')
    // Every image goes through Starr: no foreign host.
    for (const el of Array.from(container.querySelectorAll('img'))) {
      expect(el.getAttribute('src')?.startsWith('/api/v1/images/')).toBe(true)
    }
  })
})
