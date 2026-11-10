import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { CategorySettings } from '@/api/categories'
import { ok, renderWithProviders } from '@/test/render'

import CategoriesPage from './CategoriesPage'

const api = vi.hoisted(() => ({
  GET: vi.fn(),
  POST: vi.fn(),
  PUT: vi.fn(),
  PATCH: vi.fn(),
  DELETE: vi.fn(),
}))
vi.mock('@/api/client', () => ({ api }))

/** The page at /admin/media/categories/:category, the category part of the path. */
function renderCategories(category: string) {
  return renderWithProviders(
    <Routes>
      <Route path="/admin/media/categories/:category" element={<CategoriesPage />} />
    </Routes>,
    { path: `/admin/media/categories/${category}` },
  )
}

const DEFAULTS = {
  originalLanguages: [],
  providerRegion: null,
  providers: [],
  releaseRegion: null,
  excludedGenres: [],
}

function category(name: CategorySettings['category'], requestFlow: boolean): CategorySettings {
  return {
    category: name,
    enabled: !requestFlow,
    requestFlow,
    catalogFlow: requestFlow || name === 'music',
    // Music (Lidarr) and adult (Whisparr) take a source before requests.
    sourceFlow: true,
    discoverReady: false,
    requestsReady: false,
    catalogs: [],
    source: null,
    consumers: requestFlow
      ? []
      : [{ id: 4, serviceId: 2, serviceName: 'Emby', adapterType: 'emby', libraries: [] }],
    defaultRelevance: DEFAULTS,
  }
}

describe('CategoriesPage', () => {
  beforeEach(() => {
    api.GET.mockImplementation((path: string) => {
      if (path === '/api/v1/categories') {
        return Promise.resolve(
          ok([
            category('movies', true),
            category('series', true),
            category('music', false),
            category('adult', false),
          ]),
        )
      }
      if (path === '/api/v1/services' || path === '/api/v1/service-types') {
        return Promise.resolve(ok([]))
      }
      throw new Error(`unexpected GET ${path}`)
    })
  })

  it('offers adult with consume roles and a source, without roadmap text (ADM-18)', async () => {
    renderCategories('adult')
    expect(await screen.findByText(/enable it for them one by one/)).toBeTruthy()
    expect(screen.queryByText(/phase \d|later step|requests later/)).toBeNull()
    expect(screen.getByText('Emby')).toBeTruthy()
    expect(screen.queryByText('Catalog')).toBeNull()
    // Whisparr can be set up already (spec 0007 slice 3).
    expect(screen.getByRole('heading', { name: 'Source' })).toBeTruthy()
    expect(screen.getByRole('switch')).toHaveProperty('disabled', false)
    // Every category can be selected.
    for (const tab of screen.getAllByRole('tab')) expect(tab).toHaveProperty('disabled', false)
  })

  it('asks before switching a category off (ADM-07)', async () => {
    api.PATCH.mockResolvedValue(ok(category('adult', false)))
    api.PUT.mockResolvedValue(ok(category('adult', false)))
    renderCategories('adult')
    fireEvent.click(await screen.findByRole('switch'))
    const dialog = await screen.findByRole('alertdialog', { name: 'Switch NSFW off?' })
    expect(dialog.textContent).toMatch(/operator statement is cleared/)
    expect(api.PATCH).not.toHaveBeenCalled()
    expect(api.PUT).not.toHaveBeenCalled()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect(api.PATCH).not.toHaveBeenCalled()
    expect(api.PUT).not.toHaveBeenCalled()
  })

  it('does not say "none connected" under a role that has a service (ADM-18)', async () => {
    renderCategories('adult')
    expect(await screen.findByText('Emby')).toBeTruthy()
    expect(screen.queryByText(/No Jellyfin or Emby connected/)).toBeNull()
  })

  it('maps libraries per server and shows those of other categories as taken', async () => {
    const base = api.GET.getMockImplementation()
    api.GET.mockImplementation((path: string, init: unknown) => {
      if (path === '/api/v1/services/{id}/libraries') {
        return Promise.resolve(
          ok({
            serviceId: 2,
            listedAt: '2026-11-02T10:00:00Z',
            complete: true,
            error: null,
            libraries: [
              { id: '12', name: 'Adult', content: 'movies', category: 'adult' },
              { id: '3', name: 'Movies', content: 'movies', category: null },
              { id: '5', name: 'Shows', content: 'series', category: null },
            ],
          }),
        )
      }
      return base?.(path, init)
    })
    api.PUT.mockResolvedValue(ok(category('music', false)))
    renderCategories('music')
    // Nothing mapped yet: say what that means.
    expect(await screen.findByText('No libraries chosen')).toBeTruthy()
    expect(screen.getByText(/members get no library here/)).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Choose libraries' }))
    const dialog = await screen.findByRole('dialog')
    expect(await within(dialog).findByText('in NSFW')).toBeTruthy()
    const adult = within(dialog).getByRole('checkbox', { name: /^Adult/ })
    expect(adult).toHaveProperty('disabled', true)
    fireEvent.click(within(dialog).getByRole('checkbox', { name: /^Movies/ }))
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save libraries' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/categories/{category}/roles/{id}/libraries', {
        params: { path: { category: 'music', id: 4 } },
        body: { libraryIds: ['3'] },
      }),
    )
  })

  it('switches adult on only with the operator statement', async () => {
    const base = api.GET.getMockImplementation()
    api.GET.mockImplementation((path: string, init: unknown) => {
      if (path === '/api/v1/categories') {
        return Promise.resolve(ok([{ ...category('adult', false), enabled: false }]))
      }
      return base?.(path, init)
    })
    api.PATCH.mockResolvedValue(
      ok({
        ...category('adult', false),
        operatorStatement: { at: '2026-11-03T10:00:00Z', by: 'root' },
      }),
    )
    renderCategories('adult')
    fireEvent.click(await screen.findByRole('switch'))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByText(/legal where I operate this server/)).toBeTruthy()
    const confirm = within(dialog).getByRole('button', { name: 'Switch on' })
    expect(confirm).toHaveProperty('disabled', true)
    expect(api.PATCH).not.toHaveBeenCalled()
    fireEvent.click(within(dialog).getByRole('checkbox', { name: 'I confirm all three' }))
    fireEvent.click(confirm)
    await waitFor(() =>
      expect(api.PATCH).toHaveBeenCalledWith('/api/v1/categories/{category}', {
        params: { path: { category: 'adult' } },
        body: { enabled: true, operatorStatement: true },
      }),
    )
  })

  it('asks for the statement when adult was switched on without one', async () => {
    renderCategories('adult')
    expect(await screen.findByText('Statement missing')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Confirm statement' }))
    expect(await screen.findByRole('dialog')).toBeTruthy()
  })

  it('removes a source only after naming what happens, and keeps its settings (ADM-02)', async () => {
    const base = api.GET.getMockImplementation()
    const movies: CategorySettings = {
      ...category('movies', true),
      enabled: true,
      requestsReady: true,
      source: {
        id: 9,
        serviceId: 3,
        serviceName: 'Radarr',
        adapterType: 'radarr',
        settings: { qualityProfileId: 4, rootFolderPath: '/movies/' },
      },
    }
    api.GET.mockImplementation((path: string, init: unknown) =>
      path === '/api/v1/categories' ? Promise.resolve(ok([movies])) : base?.(path, init),
    )
    api.DELETE.mockImplementation((_path: string, init: { params: { query?: unknown } }) =>
      Promise.resolve(
        init.params.query
          ? ok({ ...movies, source: null, requestsReady: false })
          : {
              data: undefined,
              error: {
                code: 'categories.confirm_required',
                effects: [
                  { kind: 'requestsClose', category: 'movies' },
                  { kind: 'settingsKept', category: 'movies' },
                ],
              },
              response: new Response(null, { status: 409 }),
            },
      ),
    )
    renderCategories('movies')
    const trash = await screen.findByRole('button', { name: 'Remove Radarr from this category' })
    trash.focus()
    fireEvent.click(trash)
    // Cancel keeps everything and gives focus back to the trash button.
    fireEvent.click(
      within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Cancel' }),
    )
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    await waitFor(() => expect(document.activeElement).toBe(trash))
    fireEvent.click(trash)
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Remove Radarr as the source of Movies?')).toBeTruthy()
    expect(within(dialog).getByText(/Movies stops taking requests/)).toBeTruthy()
    expect(within(dialog).getByText(/come back when you set Radarr as the source/)).toBeTruthy()
    // Cancel is the safe default.
    expect(document.activeElement?.textContent).toBe('Cancel')
    fireEvent.click(within(dialog).getByRole('button', { name: 'Remove source' }))
    await waitFor(() =>
      expect(api.DELETE).toHaveBeenLastCalledWith('/api/v1/categories/{category}/roles/{id}', {
        params: { path: { category: 'movies', id: 9 }, query: { confirm: true } },
      }),
    )
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
  })

  it('asks before removing a catalog or a server to watch in, naming what is lost (slice 12)', async () => {
    const base = api.GET.getMockImplementation()
    const movies: CategorySettings = {
      ...category('movies', true),
      enabled: true,
      catalogs: [
        {
          id: 7,
          serviceId: 1,
          serviceName: 'TMDB',
          adapterType: 'tmdb',
          isDefault: true,
          relevance: null,
        },
      ],
      consumers: [
        { id: 4, serviceId: 2, serviceName: 'Jellyfin', adapterType: 'jellyfin', libraries: [] },
      ],
    } as CategorySettings
    api.GET.mockImplementation((path: string, init: unknown) =>
      path === '/api/v1/categories' ? Promise.resolve(ok([movies])) : base?.(path, init),
    )
    api.DELETE.mockResolvedValue(ok(movies))
    renderCategories('movies')

    fireEvent.click(await screen.findByRole('button', { name: 'Remove TMDB from this category' }))
    let dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Remove TMDB as a catalog of Movies?')).toBeTruthy()
    expect(within(dialog).getByText(/relevance settings are deleted/)).toBeTruthy()
    expect(within(dialog).getByText(/Movies has no catalog then/)).toBeTruthy()
    expect(api.DELETE).not.toHaveBeenCalled()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect(api.DELETE).not.toHaveBeenCalled()

    fireEvent.click(screen.getByRole('button', { name: 'Remove Jellyfin from this category' }))
    dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Remove Jellyfin from Movies?')).toBeTruthy()
    expect(within(dialog).getByText(/libraries chosen for Movies/)).toBeTruthy()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }))
    await waitFor(() =>
      expect(api.DELETE).toHaveBeenCalledWith('/api/v1/categories/{category}/roles/{id}', {
        params: { path: { category: 'movies', id: 4 } },
      }),
    )
  })

  it('splits the settings into sections kept in the URL', async () => {
    renderCategories('movies')
    const tabs = await screen.findByRole('tablist', { name: 'Movies settings' })
    expect(
      within(tabs)
        .getAllByRole('tab')
        .map((tab) => tab.textContent),
    ).toEqual(['Services', 'Discover', 'Source'])
    fireEvent.click(within(tabs).getByRole('tab', { name: 'Source' }))
    expect(await screen.findByText('Set a source under Services first.')).toBeTruthy()
    expect(screen.queryByRole('heading', { name: 'Catalog' })).toBeNull()
  })

  it('keeps catalog and source for movies', async () => {
    renderCategories('movies')
    expect(await screen.findByText('Catalog')).toBeTruthy()
    expect(screen.getByRole('heading', { name: 'Source' })).toBeTruthy()
    expect(screen.queryByText(/Requests come with phase/)).toBeNull()
  })
})
