import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { queryClient } from './queryClient'
import { unwrap } from './errors'
import type { components } from './schema'

export type CategorySettings = components['schemas']['CategorySettings']
export type CategoryName = components['schemas']['Category']
export type RoleKind = components['schemas']['RoleKind']
export type Relevance = components['schemas']['Relevance']
export type SourceSettings = components['schemas']['SourceSettingsDto']
export type MinimumAvailability = components['schemas']['MinimumAvailability']
export type MediaKind = components['schemas']['MediaKind']
export type CatalogReference = components['schemas']['CatalogReference']
export type Provider = components['schemas']['ProviderDto']
export type PreviewItem = components['schemas']['PreviewItem']
export type SourceLists = components['schemas']['SourceListsDto']
export type SourceSwitchPreview = components['schemas']['SourceSwitchPreview']

const CATEGORIES = ['categories'] as const

/** The kind a category's catalog lists (music: albums), or null without a catalog. */
export function mediaKindOf(category: CategoryName): MediaKind | null {
  if (category === 'movies') return 'movie'
  if (category === 'series') return 'series'
  if (category === 'music') return 'album'
  return null
}

/** Artists and albums: square covers from the Cover Art Archive. */
/** The category a media kind belongs to. */
export function categoryOfKind(kind: MediaKind): CategoryName {
  switch (kind) {
    case 'movie':
      return 'movies'
    case 'series':
      return 'series'
    case 'scene':
      return 'adult'
    case 'album':
    case 'artist':
      return 'music'
  }
}

export function isMusic(kind: MediaKind): boolean {
  return kind === 'artist' || kind === 'album'
}

export function useCategories() {
  return useQuery({
    queryKey: CATEGORIES,
    queryFn: () => unwrap(api.GET('/api/v1/categories')),
  })
}

/** Every category mutation answers with the category; the list is refreshed. */
function useCategoryMutation<V>(call: (vars: V) => Promise<CategorySettings>) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: call,
    onSuccess: (updated) => {
      client.setQueryData<CategorySettings[]>(CATEGORIES, (list) =>
        list?.map((c) => (c.category === updated.category ? updated : c)),
      )
    },
  })
}

/**
 * Switch a category on or off. Adult switches on only together with the operator statement
 * (spec 0007 Q12); the session, account menu and member lists follow the gate.
 */
export function useSetCategoryEnabled(category: CategoryName) {
  return useCategoryMutation(
    async ({
      enabled,
      operatorStatement = false,
    }: {
      enabled: boolean
      operatorStatement?: boolean
    }) => {
      const updated = await unwrap(
        api.PATCH('/api/v1/categories/{category}', {
          params: { path: { category } },
          body: { enabled, operatorStatement },
        }),
      )
      // Whether the private category may be unlocked changed with it.
      if (category === 'adult') await queryClient.invalidateQueries({ queryKey: ['auth', 'me'] })
      return updated
    },
  )
}

export function useAddRole(category: CategoryName) {
  return useCategoryMutation((body: { kind: RoleKind; serviceId: number }) =>
    unwrap(
      api.POST('/api/v1/categories/{category}/roles', { params: { path: { category } }, body }),
    ),
  )
}

/**
 * Remove a role. Removing a source with consequences answers `ConfirmRequired` with the
 * `effects` (spec 0008 ADM-02); send it again with `confirm` once the admin agreed.
 */
export function useRemoveRole(category: CategoryName) {
  return useCategoryMutation(({ id, confirm }: { id: number; confirm?: boolean }) =>
    unwrap(
      api.DELETE('/api/v1/categories/{category}/roles/{id}', {
        params: { path: { category, id }, query: confirm ? { confirm } : undefined },
      }),
    ),
  )
}

export function useSetDefaultCatalog(category: CategoryName) {
  return useCategoryMutation((id: number) =>
    unwrap(
      api.POST('/api/v1/categories/{category}/roles/{id}/default', {
        params: { path: { category, id } },
      }),
    ),
  )
}

export function useSetSource(category: CategoryName) {
  return useCategoryMutation((serviceId: number) =>
    unwrap(
      api.PUT('/api/v1/categories/{category}/source', {
        params: { path: { category } },
        body: { serviceId },
      }),
    ),
  )
}

export function fetchSourceSwitchPreview(category: CategoryName, serviceId: number) {
  return unwrap(
    api.GET('/api/v1/categories/{category}/source-switch', {
      params: { path: { category }, query: { serviceId } },
    }),
  )
}

export function useSaveRelevance(category: CategoryName) {
  return useCategoryMutation(({ roleId, relevance }: { roleId: number; relevance: Relevance }) =>
    unwrap(
      api.PUT('/api/v1/categories/{category}/roles/{id}/relevance', {
        params: { path: { category, id: roleId } },
        body: relevance,
      }),
    ),
  )
}

/** The weekly cap of automatic requests per performer or studio follow (spec 0007 Q4). */
export function useSaveFollowCap(category: CategoryName) {
  return useCategoryMutation(({ roleId, perWeek }: { roleId: number; perWeek: number }) =>
    unwrap(
      api.PUT('/api/v1/categories/{category}/roles/{id}/follow-cap', {
        params: { path: { category, id: roleId } },
        body: { perWeek },
      }),
    ),
  )
}

export function useSaveSourceSettings(category: CategoryName) {
  return useCategoryMutation((body: SourceSettings) =>
    unwrap(
      api.PUT('/api/v1/categories/{category}/source/settings', {
        params: { path: { category } },
        body,
      }),
    ),
  )
}

/** Languages, regions and genres of a catalog. */
export function useCatalogReference(serviceId: number | undefined) {
  return useQuery({
    queryKey: ['catalog-reference', serviceId],
    enabled: serviceId !== undefined,
    staleTime: 60 * 60_000,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/services/{id}/catalog/reference', {
          params: { path: { id: serviceId ?? 0 } },
        }),
      ),
  })
}

/** Watch providers of a kind in a region (none without a region). */
export function useProviders(serviceId: number | undefined, kind: MediaKind, region: string) {
  return useQuery({
    queryKey: ['catalog-providers', serviceId, kind, region],
    enabled: serviceId !== undefined && region !== '',
    staleTime: 60 * 60_000,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/services/{id}/catalog/providers', {
          params: { path: { id: serviceId ?? 0 }, query: { kind, region } },
        }),
      ),
  })
}

/** The Trending lane with a relevance that is being edited. */
export function useTrendingPreview(
  serviceId: number | undefined,
  kind: MediaKind,
  relevance: Relevance,
) {
  return useQuery({
    queryKey: ['catalog-preview', serviceId, kind, relevance],
    enabled: serviceId !== undefined,
    placeholderData: keepPreviousData,
    staleTime: 5 * 60_000,
    queryFn: () =>
      unwrap(
        api.POST('/api/v1/services/{id}/catalog/preview', {
          params: { path: { id: serviceId ?? 0 } },
          body: { kind, relevance },
        }),
      ),
  })
}

/** Quality profiles and root folders as last read from a source. */
export function useSourceLists(serviceId: number, poll: boolean) {
  return useQuery({
    queryKey: ['source-lists', serviceId],
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/services/{id}/source-lists', {
          params: { path: { id: serviceId } },
        }),
      ),
    refetchInterval: poll ? 1_000 : false,
  })
}

/** Ask the worker to read the source's lists again. */
export function useRefreshSourceLists() {
  return useMutation({
    mutationFn: (serviceId: number) =>
      unwrap(
        api.POST('/api/v1/services/{id}/source-lists/refresh', {
          params: { path: { id: serviceId } },
        }),
      ),
  })
}

export type ImageSize =
  'w45' | 'w92' | 'w154' | 'w185' | 'w300' | 'w342' | 'w500' | 'w780' | 'w1280'

/** URL of a TMDB image through Starr's proxy (no TMDB host in the browser). */
export function tmdbImage(file: string, size: ImageSize) {
  return `/api/v1/images/tmdb/${size}/${file}`
}

export type CoverSize = '250' | '500' | '1200'

/**
 * URL of an album cover (release-group MBID) through Starr's proxy; the browser never contacts
 * the Cover Art Archive. Albums without a cover answer 404.
 */
export function coverImage(mbid: string, size: CoverSize) {
  return `/api/v1/images/caa/${size}/${mbid}`
}
