import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { ME_KEY, signedOut } from '@/auth/session'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'

const keys = {
  profile: ['me', 'profile'] as const,
  sessions: ['me', 'sessions'] as const,
  services: ['me', 'services'] as const,
}

export function useProfile() {
  return useQuery({ queryKey: keys.profile, queryFn: () => unwrap(api.GET('/api/v1/me')) })
}

export function useUpdateProfile() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (displayName: string) => unwrap(api.PATCH('/api/v1/me', { body: { displayName } })),
    onSuccess: (profile) => {
      client.setQueryData(keys.profile, profile)
      // The shell shows the display name from auth/me.
      void client.invalidateQueries({ queryKey: ME_KEY })
    },
  })
}

export function useChangePassword() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (body: { currentPassword: string; newPassword: string }) =>
      unwrap(api.POST('/api/v1/me/password', { body })),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.sessions })
      // The new password goes to the services that use the Starr password.
      void client.invalidateQueries({ queryKey: keys.services })
    },
  })
}

export type MyService = components['schemas']['MyService']
export type MyServiceState = components['schemas']['MyServiceState']

/** "My services" (spec 0005 M9): the member's media servers in member words. */
export function useMyServices() {
  return useQuery({
    queryKey: keys.services,
    queryFn: () => unwrap(api.GET('/api/v1/me/services')),
    // A handoff is written within seconds: refresh while something is on its
    // way, for two minutes at most (setting up can also mean an admin acts).
    refetchInterval: (query) =>
      query.state.data?.services.some((s) => s.state === 'settingUp') &&
      query.state.dataUpdateCount < 24
        ? 5_000
        : false,
  })
}

/** Hand the Starr password to Starr once ("activate", ADR 0010). */
export function useActivateServices() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (password: string) =>
      unwrap(api.POST('/api/v1/me/services/activate', { body: { password } })),
    onSuccess: (data) => client.setQueryData(keys.services, data),
  })
}

/** "Use my Starr password here" on an adopted account (spec 0005 M2). */
export function useUseStarrPassword() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ serviceId, password }: { serviceId: number; password: string }) =>
      unwrap(
        api.POST('/api/v1/me/services/{serviceId}/use-starr-password', {
          params: { path: { serviceId } },
          body: { password },
        }),
      ),
    onSuccess: (data) => client.setQueryData(keys.services, data),
  })
}

export function useSessions() {
  return useQuery({
    queryKey: keys.sessions,
    queryFn: () => unwrap(api.GET('/api/v1/me/sessions')),
  })
}

export function useRevokeSession() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: number) =>
      unwrap(api.DELETE('/api/v1/me/sessions/{id}', { params: { path: { id } } })),
    onSuccess: () => client.invalidateQueries({ queryKey: keys.sessions }),
  })
}

/** Log out everywhere, this device included; then forget everything cached. */
export function useLogOutEverywhere() {
  return useMutation({
    mutationFn: () => unwrap(api.DELETE('/api/v1/me/sessions')),
    onSuccess: signedOut,
  })
}
