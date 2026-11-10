import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { ME_KEY, switchedTo, type Me } from '@/auth/session'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'

export type Admin = components['schemas']['AdminDto']
export type TotpEnrollment = components['schemas']['TotpEnrollment']

const keys = {
  totp: ['auth', 'totp'] as const,
  admins: ['admins'] as const,
  sessions: ['admin', 'me', 'sessions'] as const,
}

/** Where the signed-in admin is logged in (spec 0008 ADM-28). */
export function useOwnSessions() {
  return useQuery({
    queryKey: keys.sessions,
    queryFn: () => unwrap(api.GET('/api/v1/admin/me/sessions')),
  })
}

export function useRevokeOwnSession() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: number) =>
      unwrap(api.DELETE('/api/v1/admin/me/sessions/{id}', { params: { path: { id } } })),
    onSuccess: () => client.invalidateQueries({ queryKey: keys.sessions }),
  })
}

/** Change the admin's own password; the other sessions end. */
export function useChangeOwnPassword() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (body: { currentPassword: string; newPassword: string }) =>
      unwrap(api.POST('/api/v1/admin/me/password', { body })),
    onSuccess: () => client.invalidateQueries({ queryKey: keys.sessions }),
  })
}

/** After a TOTP or link change, `auth/me` (shell, guards) must refetch. */
function useRefreshMe() {
  const client = useQueryClient()
  return () => {
    void client.invalidateQueries({ queryKey: ME_KEY })
    void client.invalidateQueries({ queryKey: keys.totp })
  }
}

export function useTotpStatus() {
  return useQuery({ queryKey: keys.totp, queryFn: () => unwrap(api.GET('/api/v1/auth/totp')) })
}

export function useEnrollTotp() {
  return useMutation({ mutationFn: () => unwrap(api.POST('/api/v1/auth/totp/enroll')) })
}

export function useConfirmTotp() {
  const refresh = useRefreshMe()
  return useMutation({
    mutationFn: (code: string) => unwrap(api.POST('/api/v1/auth/totp/confirm', { body: { code } })),
    onSuccess: refresh,
  })
}

export function useDisableTotp() {
  const refresh = useRefreshMe()
  return useMutation({
    mutationFn: (body: { password: string; code: string }) =>
      unwrap(api.POST('/api/v1/auth/totp/disable', { body })),
    onSuccess: refresh,
  })
}

export function useRegenerateRecoveryCodes() {
  const refresh = useRefreshMe()
  return useMutation({
    mutationFn: (body: { password: string; code: string }) =>
      unwrap(api.POST('/api/v1/auth/totp/recovery-codes', { body })),
    onSuccess: refresh,
  })
}

export function useSetLinkedMember() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (body: { memberId: number | null; memberPassword?: string }) =>
      unwrap(api.PUT('/api/v1/auth/linked-member', { body })),
    onSuccess: (me) => {
      client.setQueryData(ME_KEY, me)
      void client.invalidateQueries({ queryKey: keys.admins })
    },
  })
}

/** Admin → linked member view, or back (after {@link elevate}). Rotates the session. */
export async function switchAccount(): Promise<Me> {
  const me = await unwrap(api.POST('/api/v1/auth/switch'))
  switchedTo(me)
  return me
}

/** Re-authenticate as the admin behind the member view (password, plus code with TOTP). */
export async function elevate(body: { password: string; code?: string }): Promise<void> {
  await unwrap(api.POST('/api/v1/auth/elevate', { body }))
}

export function useAdmins() {
  return useQuery({ queryKey: keys.admins, queryFn: () => unwrap(api.GET('/api/v1/admins')) })
}

export function useCreateAdmin() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (body: { username: string; displayName?: string }) =>
      unwrap(api.POST('/api/v1/admins', { body })),
    onSuccess: () => client.invalidateQueries({ queryKey: keys.admins }),
  })
}

export function useAdminAction() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ id, action }: { id: number; action: 'suspend' | 'reactivate' }) => {
      const params = { params: { path: { id } } }
      return action === 'suspend'
        ? unwrap(api.POST('/api/v1/admins/{id}/suspend', params))
        : unwrap(api.POST('/api/v1/admins/{id}/reactivate', params))
    },
    onSuccess: () => client.invalidateQueries({ queryKey: keys.admins }),
  })
}

export function useRemoveAdmin() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ id, username }: { id: number; username: string }) =>
      unwrap(api.DELETE('/api/v1/admins/{id}', { params: { path: { id } }, body: { username } })),
    onSuccess: () => client.invalidateQueries({ queryKey: keys.admins }),
  })
}
