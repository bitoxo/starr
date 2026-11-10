import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import type { CategoryName, CategorySettings } from './categories'
import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'

export type ServiceLibraries = components['schemas']['ServiceLibraries']
export type Library = components['schemas']['LibraryDto']
export type UnmappedLibrary = components['schemas']['UnmappedLibrary']
export type MemberInstanceLibraries = components['schemas']['MemberInstanceLibraries']

const UNMAPPED = ['libraries', 'unmapped'] as const
const serviceKey = (serviceId: number) => ['services', serviceId, 'libraries'] as const

/** A media server's libraries as last listed by the account scan. */
export function useServiceLibraries(serviceId: number, poll = false) {
  return useQuery({
    queryKey: serviceKey(serviceId),
    refetchInterval: poll ? 1_000 : false,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/services/{id}/libraries', {
          params: { path: { id: serviceId } },
        }),
      ),
  })
}

/** Save which libraries of a consume instance belong to a category. */
export function useSetRoleLibraries() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      category,
      roleId,
      libraryIds,
    }: {
      category: CategoryName
      roleId: number
      libraryIds: string[]
    }) =>
      unwrap(
        api.PUT('/api/v1/categories/{category}/roles/{id}/libraries', {
          params: { path: { category, id: roleId } },
          body: { libraryIds },
        }),
      ),
    onSuccess: (updated) => {
      client.setQueryData<CategorySettings[]>(['categories'], (list) =>
        list?.map((c) => (c.category === updated.category ? updated : c)),
      )
      void client.invalidateQueries({ queryKey: ['libraries'] })
      void client.invalidateQueries({ queryKey: ['services'] })
      void client.invalidateQueries({ queryKey: ['members'] })
    },
  })
}

/** Mark a library as belonging to no category, or undo that. */
export function useSetLibraryIgnored() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      serviceId,
      libraryId,
      ignored,
    }: {
      serviceId: number
      libraryId: string
      ignored: boolean
    }) =>
      unwrap(
        api.PUT('/api/v1/services/{id}/libraries/{libraryId}/ignored', {
          params: { path: { id: serviceId, libraryId } },
          body: { ignored },
        }),
      ),
    onSuccess: (_, { serviceId }) => {
      void client.invalidateQueries({ queryKey: UNMAPPED })
      void client.invalidateQueries({ queryKey: serviceKey(serviceId) })
    },
  })
}

/** What a member would get per media server (derived, nothing written). */
export function useMemberLibraries(memberId: number) {
  return useQuery({
    queryKey: ['members', memberId, 'libraries'],
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/members/{id}/libraries', {
          params: { path: { id: memberId } },
        }),
      ),
  })
}
