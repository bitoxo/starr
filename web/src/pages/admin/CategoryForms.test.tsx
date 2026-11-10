import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { CategorySettings } from '@/api/categories'
import { fail, ok, renderWithProviders } from '@/test/render'

import { RelevanceEditor, SourceSettingsForm } from './CategoryForms'

const api = vi.hoisted(() => ({
  GET: vi.fn(),
  POST: vi.fn(),
  PUT: vi.fn(),
  PATCH: vi.fn(),
  DELETE: vi.fn(),
}))
vi.mock('@/api/client', () => ({ api }))

const REFERENCE = {
  languages: [
    { code: 'en', name: 'English' },
    { code: 'ko', name: 'Korean' },
  ],
  countries: [
    { code: 'CH', name: 'Switzerland' },
    { code: 'DE', name: 'Germany' },
  ],
  providerRegions: [{ code: 'CH', name: 'Switzerland' }],
  genres: [
    { kind: 'series', id: 10767, name: 'Talk' },
    { kind: 'series', id: 18, name: 'Drama' },
    { kind: 'movie', id: 27, name: 'Horror' },
  ],
}

const CATALOG: CategorySettings['catalogs'][number] = {
  id: 7,
  serviceId: 3,
  serviceName: 'TMDB',
  adapterType: 'tmdb',
  isDefault: true,
  relevance: null,
}

const SERIES_DEFAULTS = {
  originalLanguages: [],
  providerRegion: null,
  providers: [],
  releaseRegion: null,
  excludedGenres: [10767],
}

function settingsWith(relevance: unknown): CategorySettings {
  return {
    category: 'series',
    enabled: true,
    requestFlow: true,
    catalogFlow: true,
    sourceFlow: true,
    discoverReady: relevance !== null,
    requestsReady: false,
    catalogs: [
      { ...CATALOG, relevance: relevance as CategorySettings['catalogs'][0]['relevance'] },
    ],
    source: null,
    consumers: [],
    defaultRelevance: SERIES_DEFAULTS,
  }
}

/** The button named `name`, once it can be used. */
async function enabledButton(name: string) {
  const button = await screen.findByRole('button', { name })
  await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false))
  return button
}

function routeGet(path: string) {
  if (path === '/api/v1/services/{id}/catalog/reference') return Promise.resolve(ok(REFERENCE))
  if (path === '/api/v1/services/{id}/catalog/providers') {
    return Promise.resolve(ok([{ id: 8, name: 'Netflix', logo: 'netflix.png' }]))
  }
  throw new Error(`unexpected GET ${path}`)
}

describe('relevance editor', () => {
  beforeEach(() => {
    api.GET.mockReset().mockImplementation(routeGet)
    api.POST.mockReset().mockResolvedValue(
      ok([
        { id: '496243', title: 'Parasite', year: 2019, originalLanguage: 'ko', poster: 'p.jpg' },
      ]),
    )
    api.PUT.mockReset()
  })

  it('starts from the series defaults and saves the edited relevance', async () => {
    api.PUT.mockResolvedValueOnce(ok(settingsWith(SERIES_DEFAULTS)))
    renderWithProviders(
      <RelevanceEditor
        category="series"
        kind="series"
        catalog={CATALOG}
        defaults={SERIES_DEFAULTS}
      />,
    )
    // Talk is hidden by default for series.
    const talk = await screen.findByRole('button', { name: 'Talk' })
    expect(talk.getAttribute('aria-pressed')).toBe('true')
    fireEvent.click(screen.getByRole('button', { name: 'Drama' }))

    fireEvent.click(await enabledButton('Original languages'))
    fireEvent.click(await screen.findByRole('option', { name: /Korean/ }))
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' })
    fireEvent.change(screen.getByLabelText('Release region'), { target: { value: 'CH' } })
    fireEvent.change(screen.getByLabelText('Streaming region'), { target: { value: 'CH' } })
    fireEvent.click(await enabledButton('Streaming services'))
    fireEvent.click(await screen.findByRole('option', { name: /Netflix/ }))

    fireEvent.click(await enabledButton('Save and turn on Discover'))
    await waitFor(() => expect(api.PUT).toHaveBeenCalled())
    expect(api.PUT).toHaveBeenCalledWith('/api/v1/categories/{category}/roles/{id}/relevance', {
      params: { path: { category: 'series', id: 7 } },
      body: {
        originalLanguages: ['ko'],
        providerRegion: 'CH',
        providers: [8],
        releaseRegion: 'CH',
        excludedGenres: [10767, 18],
      },
    })
    // Provider logos come through Starr's image proxy, never from TMDB.
    const logo = document.querySelector('img[src*="netflix.png"]')
    expect(logo?.getAttribute('src')).toBe('/api/v1/images/tmdb/w45/netflix.png')
  })

  it('shows why the server refused the relevance', async () => {
    api.PUT.mockResolvedValueOnce(fail('validation.relevance_language', 422))
    renderWithProviders(
      <RelevanceEditor
        category="series"
        kind="series"
        catalog={{ ...CATALOG, relevance: SERIES_DEFAULTS }}
        defaults={SERIES_DEFAULTS}
      />,
    )
    fireEvent.click(await enabledButton('Save relevance'))
    expect((await screen.findByRole('alert')).textContent).toContain("isn't in TMDB's list")
  })

  it('previews the Trending lane with the relevance being edited', async () => {
    renderWithProviders(
      <RelevanceEditor
        category="series"
        kind="series"
        catalog={CATALOG}
        defaults={SERIES_DEFAULTS}
      />,
    )
    const lane = await screen.findByRole('list', { name: 'Trending preview' })
    await within(lane).findByText('Parasite')
    expect(lane.querySelector('img')?.getAttribute('src')).toBe('/api/v1/images/tmdb/w342/p.jpg')
    fireEvent.click(await enabledButton('Original languages'))
    fireEvent.click(await screen.findByRole('option', { name: /Korean/ }))
    await waitFor(
      () =>
        expect(api.POST).toHaveBeenLastCalledWith('/api/v1/services/{id}/catalog/preview', {
          params: { path: { id: 3 } },
          body: {
            kind: 'series',
            relevance: { ...SERIES_DEFAULTS, originalLanguages: ['ko'] },
          },
        }),
      { timeout: 2_000 },
    )
  })
})

const LISTS = {
  qualityProfiles: [
    { id: 1, name: 'Any' },
    { id: 4, name: 'HD-1080p' },
  ],
  rootFolders: [
    { path: '/movies/', accessible: true, freeSpaceBytes: 1_200_000_000_000 },
    { path: '/offline/', accessible: false, freeSpaceBytes: null },
  ],
  metadataProfiles: [],
  fetchedAt: '2026-11-01T12:00:00Z',
  error: null,
  attemptedAt: '2026-11-01T12:00:00Z',
}

const SOURCE = {
  id: 9,
  serviceId: 5,
  serviceName: 'Radarr',
  adapterType: 'radarr',
  settings: { qualityProfileId: null, rootFolderPath: null, minimumAvailability: null },
}

describe('source settings', () => {
  beforeEach(() => {
    api.GET.mockReset().mockResolvedValue(ok(LISTS))
    api.POST.mockReset().mockResolvedValue(ok({ jobId: 1 }, 202))
    api.PUT.mockReset()
  })

  it('reads the lists fresh and saves profile, folder and minimum availability', async () => {
    api.PUT.mockResolvedValueOnce(ok({}))
    renderWithProviders(
      <SourceSettingsForm category="movies" source={SOURCE} requestsReady={false} />,
    )
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/services/{id}/source-lists/refresh', {
        params: { path: { id: 5 } },
      }),
    )
    const profile = await screen.findByLabelText('Quality profile')
    await within(profile).findByRole('option', { name: 'HD-1080p' })
    fireEvent.change(profile, { target: { value: '4' } })
    const folder = screen.getByLabelText('Root folder')
    // An unreachable folder can't be picked.
    const offline = within(folder).getByRole('option', { name: /offline/ }) as HTMLOptionElement
    expect(offline.disabled).toBe(true)
    fireEvent.change(folder, { target: { value: '/movies/' } })
    fireEvent.change(screen.getByLabelText('Minimum availability'), {
      target: { value: 'inCinemas' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save source settings' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/categories/{category}/source/settings', {
        params: { path: { category: 'movies' } },
        body: { qualityProfileId: 4, rootFolderPath: '/movies/', minimumAvailability: 'inCinemas' },
      }),
    )
  })

  it('has no minimum availability for series', async () => {
    api.PUT.mockResolvedValueOnce(ok({}))
    renderWithProviders(
      <SourceSettingsForm
        category="series"
        source={{ ...SOURCE, adapterType: 'sonarr' }}
        requestsReady={false}
      />,
    )
    const profile = await screen.findByLabelText('Quality profile')
    await within(profile).findByRole('option', { name: 'HD-1080p' })
    expect(screen.queryByLabelText('Minimum availability')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Save source settings' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/categories/{category}/source/settings', {
        params: { path: { category: 'series' } },
        body: { qualityProfileId: null, rootFolderPath: null, minimumAvailability: null },
      }),
    )
  })

  it('preselects the only profile and the only reachable root folder of the fresh read', async () => {
    // The stored lists offer two profiles; the fresh read only one.
    api.GET.mockReset()
      .mockResolvedValueOnce(ok(LISTS))
      .mockResolvedValue(
        ok({
          ...LISTS,
          qualityProfiles: [{ id: 6, name: 'HD-720p' }],
          attemptedAt: '2026-11-02T12:00:00Z',
        }),
      )
    renderWithProviders(
      <SourceSettingsForm category="series" source={SOURCE} requestsReady={false} />,
    )
    const profile = (await screen.findByLabelText('Quality profile')) as HTMLSelectElement
    // The fresh read arrives with the next poll.
    await waitFor(() => expect(profile.value).toBe('6'), { timeout: 3_000 })
    expect((screen.getByLabelText('Root folder') as HTMLSelectElement).value).toBe('/movies/')
  })

  it('never replaces a saved choice', async () => {
    const single = {
      ...LISTS,
      qualityProfiles: [{ id: 6, name: 'HD-720p' }],
      rootFolders: [{ path: '/tv/', accessible: true, freeSpaceBytes: null }],
    }
    api.GET.mockReset()
      .mockResolvedValueOnce(ok(single))
      .mockResolvedValue(ok({ ...single, attemptedAt: '2026-11-02T12:00:00Z' }))
    renderWithProviders(
      <SourceSettingsForm
        category="series"
        source={{
          ...SOURCE,
          settings: { qualityProfileId: 4, rootFolderPath: '/old/', minimumAvailability: null },
        }}
        requestsReady
      />,
    )
    const profile = (await screen.findByLabelText('Quality profile')) as HTMLSelectElement
    await within(profile).findByRole('option', { name: 'HD-720p' })
    // Also after the fresh read came back.
    await waitFor(() => expect(api.GET.mock.calls.length).toBeGreaterThan(1), { timeout: 3_000 })
    expect(profile.value).not.toBe('6')
    expect((screen.getByLabelText('Root folder') as HTMLSelectElement).value).not.toBe('/tv/')
  })

  it('warns that requests stay closed until profile and folder are saved', async () => {
    const { unmount } = renderWithProviders(
      <SourceSettingsForm category="movies" source={SOURCE} requestsReady={false} />,
    )
    expect((await screen.findByRole('note')).textContent).toContain(
      'Requests stay closed until a quality profile and root folder are saved',
    )
    unmount()
    renderWithProviders(
      <SourceSettingsForm
        category="movies"
        source={{
          ...SOURCE,
          settings: { qualityProfileId: 4, rootFolderPath: '/movies/', minimumAvailability: null },
        }}
        requestsReady
      />,
    )
    await screen.findByLabelText('Quality profile')
    expect(screen.queryByRole('note')).toBeNull()
  })
})

const MUSIC_LISTS = {
  ...LISTS,
  qualityProfiles: [{ id: 2, name: 'Lossless' }],
  rootFolders: [{ path: '/music/', accessible: true, freeSpaceBytes: null }],
  metadataProfiles: [
    {
      id: 1,
      name: 'Standard',
      primaryTypes: ['Album', 'EP'],
      secondaryTypes: ['Studio'],
      admitsAlbumsAndEps: true,
    },
    { id: 2, name: 'None', primaryTypes: [], secondaryTypes: [], admitsAlbumsAndEps: false },
  ],
}

const LIDARR = {
  ...SOURCE,
  serviceName: 'Lidarr',
  adapterType: 'lidarr',
}

describe('music source settings (Lidarr)', () => {
  beforeEach(() => {
    api.GET.mockReset().mockResolvedValue(ok(MUSIC_LISTS))
    api.POST.mockReset().mockResolvedValue(ok({ jobId: 1 }, 202))
    api.PUT.mockReset()
  })

  it('picks a metadata profile and warns when it drops albums or EPs', async () => {
    api.PUT.mockResolvedValueOnce(ok({}))
    renderWithProviders(
      <SourceSettingsForm
        category="music"
        source={LIDARR}
        requestFlow={false}
        requestsReady={false}
      />,
    )
    const metadata = await screen.findByLabelText('Metadata profile')
    await within(metadata).findByRole('option', { name: 'Standard' })
    expect(
      within(metadata).getByRole('option', { name: /None · drops albums or EPs/ }),
    ).toBeTruthy()
    // No minimum availability for music, and no roadmap text (ADM-18).
    expect(screen.queryByLabelText('Minimum availability')).toBeNull()
    expect(screen.queryByText(/later step/)).toBeNull()

    fireEvent.change(metadata, { target: { value: '2' } })
    expect((await screen.findByRole('alert')).textContent).toContain('may remove a requested album')
    fireEvent.change(metadata, { target: { value: '1' } })
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull())

    fireEvent.change(screen.getByLabelText('Quality profile'), { target: { value: '2' } })
    fireEvent.change(screen.getByLabelText('Root folder'), { target: { value: '/music/' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save source settings' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/categories/{category}/source/settings', {
        params: { path: { category: 'music' } },
        body: {
          qualityProfileId: 2,
          rootFolderPath: '/music/',
          minimumAvailability: null,
          metadataProfileId: 1,
        },
      }),
    )
  })

  it('shows a saved metadata profile', async () => {
    renderWithProviders(
      <SourceSettingsForm
        category="music"
        source={{
          ...LIDARR,
          settings: {
            qualityProfileId: 2,
            rootFolderPath: '/music/',
            minimumAvailability: null,
            metadataProfileId: 1,
          },
        }}
        requestFlow={false}
        requestsReady={false}
      />,
    )
    const metadata = (await screen.findByLabelText('Metadata profile')) as HTMLSelectElement
    await within(metadata).findByRole('option', { name: 'Standard' })
    expect(metadata.value).toBe('1')
  })
})

const ADULT_LISTS = {
  ...LISTS,
  qualityProfiles: [
    { id: 1, name: 'Any' },
    { id: 3, name: 'HD-1080p' },
  ],
  rootFolders: [
    { path: '/data/private', accessible: true, freeSpaceBytes: 800_000_000_000 },
    { path: '/offline', accessible: false, freeSpaceBytes: null },
  ],
}

const WHISPARR = {
  ...SOURCE,
  serviceName: 'Whisparr',
  adapterType: 'whisparr',
}

describe('adult source settings (Whisparr)', () => {
  beforeEach(() => {
    api.GET.mockReset().mockResolvedValue(ok(ADULT_LISTS))
    api.POST.mockReset().mockResolvedValue(ok({ jobId: 1 }, 202))
    api.PUT.mockReset()
  })

  it('saves profile and folder only, without roadmap text (ADM-18)', async () => {
    api.PUT.mockResolvedValueOnce(ok({}))
    renderWithProviders(
      <SourceSettingsForm
        category="adult"
        source={WHISPARR}
        requestFlow={false}
        requestsReady={false}
      />,
    )
    const profile = await screen.findByLabelText('Quality profile')
    await within(profile).findByRole('option', { name: 'HD-1080p' })
    // Neither Radarr's minimum availability nor Lidarr's metadata profile.
    expect(screen.queryByLabelText('Minimum availability')).toBeNull()
    expect(screen.queryByLabelText('Metadata profile')).toBeNull()
    expect(screen.queryByText(/later step of phase/)).toBeNull()

    fireEvent.change(profile, { target: { value: '3' } })
    fireEvent.change(screen.getByLabelText('Root folder'), { target: { value: '/data/private' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save source settings' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/categories/{category}/source/settings', {
        params: { path: { category: 'adult' } },
        body: { qualityProfileId: 3, rootFolderPath: '/data/private', minimumAvailability: null },
      }),
    )
  })
})
