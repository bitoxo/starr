import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { useEffect, useState } from 'react'
import { type NavigateFunction, useLocation, useNavigate } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type {
  AlbumDetails,
  ArtistDetails,
  DiscographyAlbum,
  MyRequest,
  TitleDetails,
  TitleRef,
} from '@/api/discover'
import type { Follow } from '@/api/follows'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { expectFocusNotOnBody } from '@/test/focus'
import { mockReducedMotion } from '@/test/motion'
import { fail, NO_JOURNEY, ok, renderWithProviders } from '@/test/render'

import { DetailView, DetailViewHost } from './DetailView'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn(), DELETE: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))
const toast = vi.hoisted(() => Object.assign(vi.fn(), { error: vi.fn() }))
vi.mock('sonner', () => ({ toast }))

function season(number: number, name: string, patch: Partial<TitleDetails['seasons'][0]> = {}) {
  return {
    number,
    name,
    episodeCount: 10,
    airDate: '2011-04-17',
    special: number === 0,
    status: 'new' as const,
    requestedByMe: false,
    ...patch,
  }
}

function series(patch: Partial<TitleDetails> = {}): TitleDetails {
  return {
    kind: 'series',
    id: '1399',
    category: 'series',
    title: 'Game of Thrones',
    originalTitle: 'Game of Thrones',
    year: 2011,
    overview: 'Seven noble families fight for control of the land of Westeros.',
    poster: 'got.jpg',
    backdrop: 'got-backdrop.jpg',
    genres: ['Drama'],
    rating: 8.46,
    voteCount: 25000,
    trailerUrl: 'https://www.youtube.com/watch?v=rlR4PJn8b8I',
    status: 'partially_available',
    requestedByMe: false,
    journeys: [],
    seasons: [
      season(1, 'Season 1', { status: 'available' }),
      season(2, 'Season 2', { status: 'requested' }),
      season(3, 'Season 3'),
    ],
    openIn: [{ service: 'jellyfin', name: 'Kino', url: 'https://watch.example.com/got' }],
    requestable: true,
    seasonCount: 3,
    seriesStatus: 'Returning Series',
    networks: ['HBO'],
    cast: [{ name: 'Emilia Clarke', character: 'Daenerys', profile: 'emilia.jpg' }],
    crew: [{ name: 'David Benioff', role: 'creator' }],
    watchProviders: [],
    details: { originalLanguage: 'en', companies: [], externalIds: { tmdb: 1399 } },
    attributions: [],
    ...patch,
  }
}

function movie(patch: Partial<TitleDetails> = {}): TitleDetails {
  return series({
    kind: 'movie',
    id: '27205',
    category: 'movies',
    title: 'Inception',
    originalTitle: 'Inception',
    status: 'new',
    seasons: [],
    seasonCount: null,
    seriesStatus: null,
    networks: [],
    openIn: [],
    runtimeMinutes: 148,
    releaseDate: '2010-07-16',
    certification: { region: 'DE', rating: '12' },
    tagline: 'Your mind is the scene of the crime.',
    crew: [{ name: 'Christopher Nolan', role: 'director' }],
    watchProviders: [
      { id: 8, kind: 'flatrate', name: 'Netflix', logo: 'netflix.png', region: 'DE' },
      { id: 2, kind: 'rent', name: 'Apple TV', logo: 'apple.png', region: 'DE' },
    ],
    attributions: [
      { data: 'watch_providers', source: 'JustWatch', url: 'https://www.justwatch.com' },
    ],
    ...patch,
  })
}

const FOLLOW: Follow = {
  id: 5,
  kind: 'series',
  titleId: '1399',
  title: 'Game of Thrones',
  year: 2011,
  poster: null,
  category: 'series',
  seasonsThrough: 2,
  createdAt: '2026-11-01T12:00:00Z',
  checkedAt: null,
}

function myRequest(patch: Partial<MyRequest> = {}): MyRequest {
  return {
    id: 9,
    kind: 'series',
    titleId: '1399',
    title: 'Game of Thrones',
    category: 'series',
    state: 'pending_approval',
    follow: false,
    canWithdraw: true,
    canCancel: true,
    canHide: false,
    hidden: false,
    createdAt: '2026-11-01T12:00:00Z',
    updatedAt: '2026-11-01T12:00:00Z',
    openIn: [],
    items: [{ season: 3, state: 'pending_approval', journey: NO_JOURNEY }],
    ...patch,
  }
}

const RADIOHEAD = 'a74b1b7f-71a5-4011-9441-d0b5e4122711'
const OK_COMPUTER = 'b1392450-e666-3926-a536-22c65f834433'
const KID_A = 'c1392450-e666-3926-a536-22c65f834433'

function disc(id: string, title: string, patch: Partial<DiscographyAlbum> = {}): DiscographyAlbum {
  return {
    id,
    title,
    year: 1997,
    releaseDate: '1997-05-21',
    releaseType: 'album',
    secondaryTypes: [],
    cover: id,
    inDefaultView: true,
    status: 'new',
    requestedByMe: false,
    ...patch,
  }
}

const ARTIST: ArtistDetails = {
  id: RADIOHEAD,
  category: 'music',
  name: 'Radiohead',
  disambiguation: '',
  cover: OK_COMPUTER,
  requestable: true,
  journeys: [],
  genres: ['alternative rock'],
  attributions: [
    {
      data: 'genres',
      source: 'MusicBrainz',
      licence: 'CC BY-NC-SA 3.0',
      url: 'https://creativecommons.org/licenses/by-nc-sa/3.0/',
    },
  ],
  albums: [
    disc(OK_COMPUTER, 'OK Computer'),
    disc(KID_A, 'Kid A', { year: 2000 }),
    disc('d1e2f3a4-b5c6-4d7e-8f9a-0b1c2d3e4f5a', 'Live', {
      secondaryTypes: ['live'],
      inDefaultView: false,
    }),
  ],
}

const ALBUM: AlbumDetails = {
  id: OK_COMPUTER,
  category: 'music',
  title: 'OK Computer',
  artist: 'Radiohead',
  artistId: RADIOHEAD,
  year: 1997,
  releaseDate: '1997-05-21',
  releaseType: 'album',
  secondaryTypes: [],
  trackCount: 2,
  disambiguation: '',
  cover: OK_COMPUTER,
  status: 'new',
  requestedByMe: false,
  requestable: true,
  journeys: [],
  openIn: [],
  label: 'Parlophone',
  tracks: [
    { disc: 1, discTitle: '', position: 1, title: 'Airbag', lengthSeconds: 284 },
    {
      disc: 1,
      discTitle: '',
      position: 2,
      title: 'Paranoid Android',
      lengthSeconds: 383,
      artist: 'Radiohead feat. Someone',
    },
  ],
  tracklistRelease: { date: '1997-05-21', country: 'GB', releaseCount: 3 },
  genres: ['alternative rock'],
  attributions: ARTIST.attributions,
}

type Routes = {
  title?: TitleDetails
  album?: AlbumDetails
  artist?: ArtistDetails
  follows?: Follow[]
  mine?: MyRequest[]
}

function serve({ title, album = ALBUM, artist = ARTIST, follows = [], mine = [] }: Routes) {
  api.GET.mockImplementation((path: string) => {
    switch (path) {
      case '/api/v1/follows':
        return Promise.resolve(ok(follows))
      case '/api/v1/requests':
        return Promise.resolve(ok(mine))
      case '/api/v1/albums/{id}':
        return Promise.resolve(ok(album))
      case '/api/v1/artists/{id}':
        return Promise.resolve(ok(artist))
      case '/api/v1/titles/series/{id}/seasons/{number}':
        return Promise.resolve(
          ok({
            number: 3,
            name: 'Season 3',
            overview: '',
            airDate: '2013-03-31',
            episodes: [
              { number: 1, name: 'Valar Dohaeris', airDate: '2013-03-31', runtimeMinutes: 55 },
            ],
          }),
        )
      default:
        return Promise.resolve(title ? ok(title) : fail('catalog.not_found', 404))
    }
  })
}

let navigate: NavigateFunction | null = null
let location = ''
function Spy() {
  const nav = useNavigate()
  const search = useLocation().search
  useEffect(() => {
    navigate = nav
    location = search
  }, [nav, search])
  return null
}

function host(path: string) {
  return renderWithProviders(
    <>
      <main>
        <h1>Discover</h1>
      </main>
      <Spy />
      <DetailViewHost />
    </>,
    { path },
  )
}

beforeEach(() => {
  api.GET.mockReset()
  api.POST.mockReset()
  api.DELETE.mockReset()
})

describe('DetailView: movie', () => {
  it('shows the facts, the request card before the trailer, providers with credit', async () => {
    serve({ title: movie() })
    host('/?title=movie-27205')
    const dialog = await screen.findByRole('dialog', { name: 'Inception' })
    expect(within(dialog).getByText('Your mind is the scene of the crime.')).toBeTruthy()
    expect(within(dialog).getByText('Rated 12')).toBeTruthy()
    expect(within(dialog).getByText('2 h 28 min')).toBeTruthy()
    const request = within(dialog).getByRole('button', { name: 'Request movie' })
    const trailer = within(dialog).getByRole('link', { name: 'Watch trailer' })
    // Request first, the trailer after it (MEM-08).
    expect(request.compareDocumentPosition(trailer) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
    expect(trailer.getAttribute('target')).toBe('_blank')
    expect(within(dialog).getByText('Netflix')).toBeTruthy()
    expect(within(dialog).getByRole('heading', { name: 'Stream' })).toBeTruthy()
    expect(within(dialog).getByRole('link', { name: 'Data from JustWatch' })).toBeTruthy()
    for (const img of Array.from(dialog.querySelectorAll('img'))) {
      expect(img.getAttribute('src')).toMatch(/^\/api\/v1\/images\//)
    }
    expect(document.title).toContain('Inception')
  })

  it('requests the movie with the one gold button', async () => {
    serve({ title: movie() })
    api.POST.mockResolvedValueOnce(
      ok({ request: myRequest({ kind: 'movie', titleId: '27205', items: [] }), skipped: [] }, 201),
    )
    host('/?title=movie-27205')
    fireEvent.click(await screen.findByRole('button', { name: 'Request movie' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests', {
        body: { kind: 'movie', titleId: '27205' },
      }),
    )
    expect(await screen.findByText("Your request waits for your admin's approval.")).toBeTruthy()
  })

  it('shows the cast, not clickable, with the director above', async () => {
    serve({ title: movie() })
    host('/?title=movie-27205')
    const dialog = await screen.findByRole('dialog', { name: 'Inception' })
    fireEvent.mouseDown(within(dialog).getByRole('tab', { name: 'Cast' }))
    fireEvent.click(within(dialog).getByRole('tab', { name: 'Cast' }))
    const panel = await within(dialog).findByRole('tabpanel', { name: 'Cast' })
    expect(within(panel).getByText('Emilia Clarke')).toBeTruthy()
    expect(within(panel).getByText('Christopher Nolan')).toBeTruthy()
    expect(within(panel).queryByRole('link')).toBeNull()
  })

  it('says when a requested movie is arriving', async () => {
    serve({ title: movie({ status: 'requested', stage: 'arriving', arrivingIn: 'Jellyfin' }) })
    host('/?title=movie-27205')
    const dialog = await screen.findByRole('dialog', { name: 'Inception' })
    expect(within(dialog).getByText('Arriving in Jellyfin')).toBeTruthy()
  })

  it('makes "Open in" the gold action once it is there, with no dead Request', async () => {
    serve({
      title: movie({
        status: 'available',
        openIn: [{ service: 'jellyfin', name: 'Kino', url: 'https://watch.example.com/x' }],
      }),
    })
    host('/?title=movie-27205')
    const dialog = await screen.findByRole('dialog', { name: 'Inception' })
    expect(within(dialog).queryByRole('button', { name: 'Request movie' })).toBeNull()
    const openIn = within(dialog).getByRole('link', { name: 'Open in Jellyfin' })
    expect(openIn.className).toContain('bg-polaris')
  })
})

describe('DetailView: series', () => {
  it('keeps available seasons out of the choice; gold only after choosing (MEM-06)', async () => {
    serve({ title: series() })
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    fireEvent.mouseDown(within(dialog).getByRole('tab', { name: 'Seasons' }))
    expect(within(dialog).queryByRole('checkbox', { name: 'Season 1' })).toBeNull()
    expect(within(dialog).getByText('Select the seasons you want to request.')).toBeTruthy()
    expect(within(dialog).queryByRole('button', { name: /^Request \d season/ })).toBeNull()
    fireEvent.click(within(dialog).getByRole('checkbox', { name: 'Season 3' }))
    const request = within(dialog).getByRole('button', { name: 'Request 1 season' })
    expect(request.className).toContain('bg-polaris')
  })

  it('says everything is available instead of a dead button', async () => {
    serve({
      title: series({
        status: 'available',
        seasons: [season(1, 'Season 1', { status: 'available' })],
      }),
    })
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    fireEvent.mouseDown(within(dialog).getByRole('tab', { name: 'Seasons' }))
    expect(within(dialog).getByText('Everything is available.')).toBeTruthy()
    expect(within(dialog).queryByRole('checkbox')).toBeNull()
  })

  it('requests seasons and keeps the list where it is (MEM-10)', async () => {
    serve({ title: series() })
    api.POST.mockResolvedValueOnce(ok({ request: myRequest(), skipped: [] }, 201))
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    fireEvent.mouseDown(within(dialog).getByRole('tab', { name: 'Seasons' }))
    fireEvent.click(within(dialog).getByRole('checkbox', { name: 'Season 3' }))
    fireEvent.click(within(dialog).getByRole('button', { name: 'Request 1 season' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests', {
        body: { kind: 'series', titleId: '1399', seasons: [3] },
      }),
    )
    expect(
      await within(dialog).findByText("Your request waits for your admin's approval."),
    ).toBeTruthy()
    expect(within(dialog).getByRole('tabpanel', { name: 'Seasons' })).toBeTruthy()
    expect(within(dialog).getByText('Season 1')).toBeTruthy()
  })

  it('opens a season’s episodes on demand', async () => {
    serve({ title: series() })
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    fireEvent.mouseDown(within(dialog).getByRole('tab', { name: 'Seasons' }))
    const toggle = within(dialog).getByRole('button', { name: 'Episodes of Season 3' })
    expect(toggle.getAttribute('aria-expanded')).toBe('false')
    fireEvent.click(toggle)
    expect(await within(dialog).findByText('Valar Dohaeris')).toBeTruthy()
    expect(toggle.getAttribute('aria-expanded')).toBe('true')
    expect(api.GET).toHaveBeenCalledWith('/api/v1/titles/series/{id}/seasons/{number}', {
      params: { path: { id: '1399', number: 3 } },
    })
  })

  it('says a season without listed episodes has none, without an error', async () => {
    serve({ title: series() })
    const served = api.GET.getMockImplementation()
    api.GET.mockImplementation((path: string, init: unknown) =>
      path === '/api/v1/titles/series/{id}/seasons/{number}'
        ? Promise.resolve(fail('catalog.not_found', 404))
        : served?.(path, init),
    )
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    fireEvent.mouseDown(within(dialog).getByRole('tab', { name: 'Seasons' }))
    fireEvent.click(within(dialog).getByRole('button', { name: 'Episodes of Season 3' }))
    expect(await within(dialog).findByText('No episodes listed yet.')).toBeTruthy()
    expect(within(dialog).queryByRole('alert')).toBeNull()
  })

  it('has one follow switch (MEM-07), off until followed', async () => {
    serve({ title: series() })
    api.POST.mockResolvedValueOnce(ok(FOLLOW, 201))
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    const switches = await within(dialog).findAllByRole('switch')
    expect(switches).toHaveLength(1)
    const toggle = within(dialog).getByRole('switch', { name: 'Follow new seasons' })
    await waitFor(() => expect((toggle as HTMLButtonElement).disabled).toBe(false))
    fireEvent.click(toggle)
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/follows', {
        body: { kind: 'series', titleId: '1399' },
      }),
    )
  })

  it('hides the follow switch for an ended series nobody follows', async () => {
    serve({ title: series({ seriesStatus: 'Ended' }) })
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    await waitFor(() => expect(api.GET).toHaveBeenCalledWith('/api/v1/follows'))
    expect(within(dialog).queryByRole('switch')).toBeNull()
  })

  it('softens it for an ended series that is followed', async () => {
    serve({ title: series({ seriesStatus: 'Ended' }), follows: [FOLLOW] })
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    expect(await within(dialog).findByText(/This series has ended/)).toBeTruthy()
    expect(within(dialog).getByRole('switch').getAttribute('data-state')).toBe('checked')
  })

  it('shows the journey inline and withdraws while it waits (MEM-05, MEM-08)', async () => {
    const waiting = {
      steps: [
        { step: 'requested' as const, status: 'done' as const },
        { step: 'awaiting_approval' as const, status: 'current' as const },
        { step: 'ready' as const, status: 'upcoming' as const },
      ],
    }
    serve({
      title: series({ requestedByMe: true, journeys: [{ season: 3, journey: waiting }] }),
      mine: [myRequest()],
    })
    api.POST.mockResolvedValueOnce(ok(myRequest({ state: 'cancelled' })))
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    const card = within(dialog).getByRole('region', { name: 'Request and status' })
    // No popover: the steps are right there.
    expect(within(card).getByText('Requested')).toBeTruthy()
    expect(within(card).queryByRole('button', { name: /Waiting for approval/ })).toBeNull()
    fireEvent.click(await within(card).findByRole('button', { name: 'Withdraw request' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests/{id}/withdraw', {
        params: { path: { id: 9 } },
      }),
    )
  })
})

describe('DetailView: withdraw after the refetch (MEM-05)', () => {
  it('says so and keeps focus in the card although the button is gone', async () => {
    const waiting = {
      steps: [
        { step: 'requested' as const, status: 'done' as const },
        { step: 'awaiting_approval' as const, status: 'current' as const },
      ],
    }
    let withdrawn = false
    api.GET.mockImplementation((path: string) => {
      if (path === '/api/v1/follows') return Promise.resolve(ok([]))
      if (path === '/api/v1/requests') return Promise.resolve(ok(withdrawn ? [] : [myRequest()]))
      return Promise.resolve(
        ok(
          withdrawn
            ? series()
            : series({ requestedByMe: true, journeys: [{ season: 3, journey: waiting }] }),
        ),
      )
    })
    api.POST.mockImplementation(() => {
      withdrawn = true
      return Promise.resolve(ok(myRequest({ state: 'cancelled' })))
    })
    toast.mockClear()
    host('/?title=series-1399')
    const dialog = await screen.findByRole('dialog', { name: 'Game of Thrones' })
    const card = within(dialog).getByRole('region', { name: 'Request and status' })
    const button = await within(card).findByRole('button', { name: 'Withdraw request' })
    act(() => button.focus())
    fireEvent.click(button)
    await waitFor(() => expect(toast).toHaveBeenCalledWith('Request withdrawn.'))
    await waitFor(() =>
      expect(within(card).queryByRole('button', { name: 'Withdraw request' })).toBeNull(),
    )
    expect(document.activeElement).toBe(card)
  })
})

describe('DetailView: history', () => {
  it('closes past the views it opened inside, so Back does not reopen it', async () => {
    serve({})
    renderWithProviders(
      <>
        <Spy />
        <PageWithOpener />
        <DetailViewHost />
      </>,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Open artist' }))
    const artist = await screen.findByRole('dialog', { name: 'Radiohead' })
    fireEvent.click(within(artist).getByRole('button', { name: /OK Computer/ }))
    const album = await screen.findByRole('dialog', { name: 'OK Computer' })
    fireEvent.keyDown(album, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    // Back went past both overlay entries: the page the overlay was opened from.
    expect(location).toBe('')
    expect(screen.getByRole('button', { name: 'Open artist' })).toBeTruthy()
  })
})

function PageWithOpener() {
  const { open } = useTitleSheet()
  return (
    <main>
      <h1>Discover</h1>
      <button type="button" onClick={() => open({ kind: 'artist', id: RADIOHEAD })}>
        Open artist
      </button>
    </main>
  )
}

describe('DetailView: music', () => {
  it('shows an album’s tracklist first, its artist as a link, genres with credit', async () => {
    serve({})
    host(`/?title=album-${OK_COMPUTER}`)
    const dialog = await screen.findByRole('dialog', { name: 'OK Computer' })
    const tracks = within(dialog).getByRole('tabpanel', { name: 'Tracks' })
    expect(within(tracks).getByText('Paranoid Android')).toBeTruthy()
    expect(within(tracks).getByText('Radiohead feat. Someone')).toBeTruthy()
    expect(within(tracks).getByText('4 min 44 s')).toBeTruthy()
    expect(within(tracks).getByText('Tracklist of the 1997 release')).toBeTruthy()
    fireEvent.mouseDown(within(dialog).getByRole('tab', { name: 'Details' }))
    expect(
      within(dialog).getByRole('link', { name: 'Genres from MusicBrainz (CC BY-NC-SA 3.0)' }),
    ).toBeTruthy()
    fireEvent.click(within(dialog).getAllByRole('button', { name: 'Radiohead' })[0]!)
    await waitFor(() => expect(location).toBe(`?title=artist-${RADIOHEAD}`))
  })

  it('requests an album', async () => {
    serve({})
    api.POST.mockResolvedValueOnce(
      ok({ request: myRequest({ kind: 'artist', titleId: RADIOHEAD, items: [] }), skipped: [] }),
    )
    host(`/?title=album-${OK_COMPUTER}`)
    fireEvent.click(await screen.findByRole('button', { name: 'Request album' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/requests', {
        body: { kind: 'album', titleId: OK_COMPUTER },
      }),
    )
  })

  it('keeps an artist’s ticks and "Show all" after visiting an album (MEM-29)', async () => {
    serve({})
    host(`/?title=artist-${RADIOHEAD}`)
    const dialog = await screen.findByRole('dialog', { name: 'Radiohead' })
    fireEvent.click(within(dialog).getByRole('checkbox', { name: 'Choose Kid A' }))
    fireEvent.click(within(dialog).getByRole('button', { name: /Show all/ }))
    expect(within(dialog).getByRole('button', { name: 'Request 1 album' })).toBeTruthy()
    fireEvent.click(within(dialog).getByRole('button', { name: /OK Computer/ }))
    const album = await screen.findByRole('dialog', { name: 'OK Computer' })
    // The tile that had focus is gone with the artist view: the album's first tab takes it.
    await waitFor(() =>
      expect(document.activeElement).toBe(within(album).getByRole('tab', { name: 'Tracks' })),
    )
    act(() => navigate?.(-1))
    const back = await screen.findByRole('dialog', { name: 'Radiohead' })
    expect(
      within(back).getByRole('checkbox', { name: 'Choose Kid A' }).getAttribute('data-state'),
    ).toBe('checked')
    expect(within(back).getByRole('button', { name: 'Show fewer' })).toBeTruthy()
  })
})

describe('DetailView: not found (MEM-32)', () => {
  it('says so at once for a malformed ?title=', async () => {
    serve({})
    host('/?title=nonsense')
    expect(await screen.findByRole('dialog', { name: "This title isn't here" })).toBeTruthy()
    expect(api.GET).not.toHaveBeenCalled()
  })

  it('says so after one 404, without retrying', async () => {
    serve({})
    host('/?title=movie-999999')
    expect(await screen.findByRole('dialog', { name: "This title isn't here" })).toBeTruthy()
    const calls = api.GET.mock.calls.filter(([path]) => path === '/api/v1/titles/{kind}/{id}')
    expect(calls).toHaveLength(1)
    // Its own button, beside the corner X.
    fireEvent.click(screen.getAllByRole('button', { name: 'Close' }).at(-1)!)
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(location).toBe('')
  })
})

describe('DetailView: focus (spec 0008 F3)', () => {
  function Opener({ data, deepLink = false }: { data: TitleDetails; deepLink?: boolean }) {
    const [ref, setRef] = useState<TitleRef | null>(
      deepLink ? { kind: data.kind, id: data.id } : null,
    )
    return (
      <main>
        <h1>Discover</h1>
        {!deepLink && (
          <button type="button" onClick={() => setRef({ kind: data.kind, id: data.id })}>
            Poster
          </button>
        )}
        <DetailView titleRef={ref} open={ref !== null} onClose={() => setRef(null)} />
      </main>
    )
  }

  it('puts focus on the first tab, and back on the poster after Esc', async () => {
    serve({ title: movie() })
    renderWithProviders(<Opener data={movie()} />)
    const poster = screen.getByRole('button', { name: 'Poster' })
    act(() => poster.focus())
    fireEvent.click(poster)
    const dialog = await screen.findByRole('dialog', { name: 'Inception' })
    await waitFor(() =>
      expect(document.activeElement).toBe(within(dialog).getByRole('tab', { name: 'Overview' })),
    )
    fireEvent.keyDown(dialog, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(document.activeElement).toBe(poster)
  })

  it('focuses the first tab at once when the title is cached', async () => {
    serve({ title: movie() })
    renderWithProviders(<Opener data={movie()} />)
    const poster = screen.getByRole('button', { name: 'Poster' })
    fireEvent.click(poster)
    const dialog = await screen.findByRole('dialog', { name: 'Inception' })
    fireEvent.keyDown(dialog, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    // Open again: the data is in the cache now, the tabs render in the first commit.
    fireEvent.click(poster)
    const again = await screen.findByRole('dialog', { name: 'Inception' })
    await waitFor(() =>
      expect(document.activeElement).toBe(within(again).getByRole('tab', { name: 'Overview' })),
    )
  })

  it('gives focus back to the poster when the title was cached (slice 12)', async () => {
    serve({ title: movie() })
    renderWithProviders(<Opener data={movie()} />)
    const poster = screen.getByRole('button', { name: 'Poster' })
    act(() => poster.focus())
    fireEvent.click(poster)
    fireEvent.keyDown(await screen.findByRole('dialog', { name: 'Inception' }), { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    // The tabs render in the first commit now and take focus before Radix's mount autofocus.
    act(() => poster.focus())
    fireEvent.click(poster)
    fireEvent.keyDown(await screen.findByRole('dialog', { name: 'Inception' }), { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(document.activeElement).toBe(poster)
  })

  it('lands on the page heading after a deep link', async () => {
    serve({ title: movie() })
    renderWithProviders(<Opener data={movie()} deepLink />)
    fireEvent.keyDown(await screen.findByRole('dialog', { name: 'Inception' }), {
      key: 'Escape',
    })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'Discover' }))
    expectFocusNotOnBody()
  })
})

describe('DetailView: motion (A2)', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    // @ts-expect-error: jsdom has no view transitions; the tests add and remove one.
    delete document.startViewTransition
  })

  function PosterOpener() {
    const { open } = useTitleSheet()
    return (
      <button type="button" onClick={() => open({ kind: 'movie', id: '27205' })}>
        <img src="/api/v1/images/tmdb/w342/x.jpg" alt="" />
        Poster
      </button>
    )
  }

  function clickPoster() {
    const poster = screen.getByRole('button', { name: 'Poster' })
    act(() => poster.focus())
    fireEvent.click(poster)
    return poster.querySelector('img')
  }

  it('morphs the poster into the cover where view transitions exist', () => {
    mockReducedMotion(false)
    document.startViewTransition = vi.fn() as unknown as typeof document.startViewTransition
    renderWithProviders(<PosterOpener />)
    expect(clickPoster()?.style.getPropertyValue('view-transition-name')).toBe('detail-cover')
  })

  it('does not move anything with reduced motion', () => {
    mockReducedMotion(true)
    document.startViewTransition = vi.fn() as unknown as typeof document.startViewTransition
    renderWithProviders(<PosterOpener />)
    expect(clickPoster()?.style.getPropertyValue('view-transition-name')).toBe('')
  })
})
