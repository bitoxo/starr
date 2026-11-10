import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'

export type Member = components['schemas']['MemberDto']
export type MemberDetail = components['schemas']['MemberDetail']
export type MemberStatus = components['schemas']['MemberStatus']
export type Entitlement = components['schemas']['Entitlement']
export type CategoryName = components['schemas']['Category']
export type RequestMode = components['schemas']['RequestMode']
export type Link = components['schemas']['LinkDto']
export type SessionInfo = components['schemas']['SessionDto']
export type Invitation = components['schemas']['InvitationDto']

/** Category order everywhere in the UI (matches the server). */
export const CATEGORIES: CategoryName[] = ['movies', 'series', 'music', 'adult']

/** One entry per category; adult off and approval by default. */
export function defaultEntitlements(): Entitlement[] {
  return CATEGORIES.map((category) => ({ category, enabled: false, requestMode: 'approval' }))
}

export interface MemberFilter {
  status?: MemberStatus
  q?: string
}

const keys = {
  all: ['members'] as const,
  list: (filter: MemberFilter) => ['members', 'list', filter] as const,
  detail: (id: number) => ['members', 'detail', id] as const,
  invitations: ['invitations'] as const,
}

export function useMembers(filter: MemberFilter) {
  return useQuery({
    queryKey: keys.list(filter),
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/members', {
          params: { query: { status: filter.status, q: filter.q || undefined } },
        }),
      ),
    placeholderData: (previous) => previous,
  })
}

export function useMember(id: number) {
  return useQuery({
    queryKey: keys.detail(id),
    queryFn: () => unwrap(api.GET('/api/v1/members/{id}', { params: { path: { id } } })),
  })
}

/** Invalidate member lists and details after any member write. */
function useInvalidateMembers() {
  const client = useQueryClient()
  return () => client.invalidateQueries({ queryKey: keys.all })
}

export function useCreateMember() {
  const invalidate = useInvalidateMembers()
  return useMutation({
    mutationFn: (body: {
      username: string
      email: string
      displayName?: string
      entitlements: Entitlement[]
    }) => unwrap(api.POST('/api/v1/members', { body })),
    onSuccess: invalidate,
  })
}

type MemberAction = 'suspend' | 'reactivate' | 'restore'

export function useMemberAction(id: number) {
  const invalidate = useInvalidateMembers()
  return useMutation({
    mutationFn: (action: MemberAction) => {
      const params = { params: { path: { id } } }
      switch (action) {
        case 'suspend':
          return unwrap(api.POST('/api/v1/members/{id}/suspend', params))
        case 'reactivate':
          return unwrap(api.POST('/api/v1/members/{id}/reactivate', params))
        case 'restore':
          return unwrap(api.POST('/api/v1/members/{id}/restore', params))
      }
    },
    onSuccess: invalidate,
  })
}

export function useResetPassword(id: number) {
  return useMutation({
    mutationFn: () =>
      unwrap(api.POST('/api/v1/members/{id}/password-reset', { params: { path: { id } } })),
  })
}

export function useDeleteMember(id: number) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (username: string) =>
      unwrap(api.DELETE('/api/v1/members/{id}', { params: { path: { id } }, body: { username } })),
    onSuccess: (result) => {
      void client.invalidateQueries({ queryKey: ['members', 'list'] })
      // A deleted member's detail would refetch into a 404; the page navigates away.
      if (result.outcome === 'scheduled') {
        void client.invalidateQueries({ queryKey: keys.detail(id) })
        // The account plan now disables instead of creating (ADM-13): refetch it.
        void client.invalidateQueries({ queryKey: ['members', id, 'account-plan'] })
      }
    },
  })
}

export function useSaveEntitlements(id: number) {
  const invalidate = useInvalidateMembers()
  return useMutation({
    mutationFn: (entitlements: Entitlement[]) =>
      unwrap(
        api.PUT('/api/v1/members/{id}/entitlements', {
          params: { path: { id } },
          body: { entitlements },
        }),
      ),
    onSuccess: invalidate,
  })
}

export function useInvitations() {
  return useQuery({
    queryKey: keys.invitations,
    queryFn: () => unwrap(api.GET('/api/v1/invitations')),
  })
}

export function useCreateInvitation() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (body: {
      expiresInDays: number
      username?: string
      email?: string
      entitlements: Entitlement[]
      accounts?: { serviceId: number; remoteId: string }[]
    }) => unwrap(api.POST('/api/v1/invitations', { body })),
    onSuccess: () => client.invalidateQueries({ queryKey: keys.invitations }),
  })
}

export function useRevokeInvitation() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: number) =>
      unwrap(api.POST('/api/v1/invitations/{id}/revoke', { params: { path: { id } } })),
    onSuccess: () => client.invalidateQueries({ queryKey: keys.invitations }),
  })
}

/** An absolute link to copy: the server returns a path when STARR_PUBLIC_URL is unset. */
export function absoluteUrl(url: string): string {
  return url.startsWith('/') ? `${window.location.origin}${url}` : url
}
