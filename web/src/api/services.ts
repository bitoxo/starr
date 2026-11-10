import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'
import { neutralHeld } from '@/lib/privacy'

export type Service = components['schemas']['Service']
export type ServiceType = components['schemas']['ServiceType']
export type CreateServiceInput = components['schemas']['CreateService']
export type UpdateServiceInput = components['schemas']['UpdateService']

const SERVICES = ['services'] as const

/** All service instances. Polls while an instance waits for its first check. */
export function useServices() {
  return useQuery({
    queryKey: SERVICES,
    queryFn: async () => {
      const data = await unwrap(api.GET('/api/v1/services'))
      return data
    },
    refetchInterval: (query) =>
      query.state.data?.some((s) => s.lastCheckAt === null) ? 2_000 : false,
  })
}

export function useServiceTypes() {
  return useQuery({
    queryKey: ['service-types'],
    queryFn: async () => {
      const data = await unwrap(api.GET('/api/v1/service-types'))
      return data
    },
    staleTime: Infinity,
  })
}

export function useCreateService() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: async (body: CreateServiceInput) => {
      const data = await unwrap(api.POST('/api/v1/services', { body }))
      return data
    },
    onSuccess: () => client.invalidateQueries({ queryKey: SERVICES }),
  })
}

export function useUpdateService() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: async ({ id, body }: { id: number; body: UpdateServiceInput }) => {
      const data = await unwrap(
        api.PATCH('/api/v1/services/{id}', {
          params: { path: { id } },
          body,
        }),
      )
      return data
    },
    onSuccess: () => client.invalidateQueries({ queryKey: SERVICES }),
  })
}

export function useDeleteService() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: async (id: number) => {
      await unwrap(
        api.DELETE('/api/v1/services/{id}', {
          params: { path: { id } },
        }),
      )
    },
    onSuccess: () => client.invalidateQueries({ queryKey: SERVICES }),
  })
}

export function useCheckService() {
  return useMutation({
    mutationFn: async (id: number) => {
      const data = await unwrap(
        api.POST('/api/v1/services/{id}/check', {
          params: { path: { id } },
        }),
      )
      return data.jobId
    },
  })
}

/** Pause or resume writes to a source; held acquisitions change with it. */
export function useSetWritesPaused() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: async ({ id, paused }: { id: number; paused: boolean }) => {
      const data = await unwrap(
        api.PUT('/api/v1/services/{id}/writes-paused', {
          params: { path: { id } },
          body: { paused },
        }),
      )
      return data
    },
    onSuccess: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: SERVICES }),
        client.invalidateQueries({ queryKey: ['requests'] }),
      ]),
  })
}

export type HeldAcquisitions = components['schemas']['HeldAcquisitions']
export type HeldTitle = components['schemas']['HeldTitle']

/** Reads what resuming a paused source sends out, fresh, right before asking. */
const heldQuery = (id: number) => ({
  queryKey: [...SERVICES, id, 'held'],
  queryFn: async () => {
    const held = await unwrap(api.GET('/api/v1/services/{id}/held', { params: { path: { id } } }))
    return { ...held, preview: neutralHeld(held.preview), stops: neutralHeld(held.stops) }
  },
})

export function useFetchHeld() {
  const client = useQueryClient()
  return (id: number) => client.fetchQuery({ ...heldQuery(id), staleTime: 0 })
}

/** What waits at a paused source, for the count beside its writes switch (ADM-15). */
export function useHeld(id: number, enabled: boolean) {
  return useQuery({ ...heldQuery(id), enabled })
}
