import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'

export type Notification = components['schemas']['NotificationDto']
export type NotificationKind = components['schemas']['NotificationKind']
export type NotificationPage = components['schemas']['NotificationPage']
export type NotificationPreferences = components['schemas']['NotificationPreferences']
export type Channel = components['schemas']['ChannelDto']
export type ChannelKind = components['schemas']['NotificationChannelKind']
export type EmailSettings = components['schemas']['EmailSettingsDto']
export type SmtpSecurity = components['schemas']['SmtpSecurity']
export type CreateChannel = components['schemas']['CreateChannel']
export type UpdateChannel = components['schemas']['UpdateChannel']

/** Whose stream: the logged-in member's, or the shared admin stream. */
export type Scope = 'member' | 'admin'

/** Kinds of the shared admin stream; every other kind is a member's. */
export type AdminKind = Extract<
  NotificationKind,
  | 'approval_pending'
  | 'acquisition_failed'
  | 'import_blocked'
  | 'account_privileged'
  | 'account_locked'
>
export type MemberKind = Exclude<NotificationKind, AdminKind>
// `satisfies` makes a new kind from the API fail the typecheck until it is
// listed here, in display order (lesson 16).
const MEMBER_ORDER = {
  account_activate: true,
  follow_requested: true,
  follow_capped: true,
  request_approved: true,
  request_rejected: true,
  request_revoked: true,
  request_partially_available: true,
  request_available: true,
  request_failed: true,
} satisfies Record<MemberKind, true>
const ADMIN_ORDER = {
  approval_pending: true,
  acquisition_failed: true,
  import_blocked: true,
  account_privileged: true,
  account_locked: true,
} satisfies Record<AdminKind, true>
export const MEMBER_KINDS = Object.keys(MEMBER_ORDER) as MemberKind[]
export const ADMIN_KINDS = Object.keys(ADMIN_ORDER) as AdminKind[]

const KEY = ['notifications'] as const

/** The newest notifications of a stream with its unread count. Polls only while visible. */
export function useNotifications(scope: Scope) {
  return useQuery({
    queryKey: [...KEY, scope],
    queryFn: () =>
      unwrap(
        scope === 'admin'
          ? api.GET('/api/v1/admin/notifications', { params: { query: { limit: 30 } } })
          : api.GET('/api/v1/notifications', { params: { query: { limit: 30 } } }),
      ),
    refetchInterval: 30_000,
  })
}

/**
 * The member's whole stream, page by page (spec 0008 slice 6, MEM-21): newest first, the next
 * page from `nextBefore`. Under the `notifications` key, so marking read refreshes it and the
 * private gate drops its private entries on close.
 */
export function useNotificationHistory() {
  return useInfiniteQuery({
    queryKey: [...KEY, 'member', 'history'],
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET('/api/v1/notifications', {
          params: { query: pageParam == null ? { limit: 30 } : { limit: 30, before: pageParam } },
        }),
      ),
    initialPageParam: null as number | null,
    getNextPageParam: (last) => last.nextBefore ?? null,
  })
}

/** Mark the given notifications read, or all of the stream without ids. */
export function useMarkRead(scope: Scope) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (ids?: number[]) => {
      const body = ids ? { ids } : {}
      return unwrap(
        scope === 'admin'
          ? api.POST('/api/v1/admin/notifications/read', { body })
          : api.POST('/api/v1/notifications/read', { body }),
      )
    },
    onSuccess: () => client.invalidateQueries({ queryKey: [...KEY, scope] }),
  })
}

const PREFS = [...KEY, 'preferences'] as const

export function useNotificationPreferences() {
  return useQuery({
    queryKey: PREFS,
    queryFn: () => unwrap(api.GET('/api/v1/me/notification-preferences')),
  })
}

export function useUpdateNotificationPreferences() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (email: NotificationKind[]) =>
      unwrap(api.PUT('/api/v1/me/notification-preferences', { body: { email } })),
    onSuccess: (prefs) => client.setQueryData(PREFS, prefs),
  })
}

const CHANNELS = [...KEY, 'channels'] as const

export function useChannels() {
  return useQuery({
    queryKey: CHANNELS,
    queryFn: () => unwrap(api.GET('/api/v1/admin/notification-channels')),
  })
}

export function useCreateChannel() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (body: CreateChannel) =>
      unwrap(api.POST('/api/v1/admin/notification-channels', { body })),
    onSuccess: () => client.invalidateQueries({ queryKey: CHANNELS }),
  })
}

export function useUpdateChannel() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ id, body }: { id: number; body: UpdateChannel }) =>
      unwrap(
        api.PATCH('/api/v1/admin/notification-channels/{id}', {
          params: { path: { id } },
          body,
        }),
      ),
    onSuccess: () => client.invalidateQueries({ queryKey: CHANNELS }),
  })
}

export function useDeleteChannel() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: number) =>
      unwrap(api.DELETE('/api/v1/admin/notification-channels/{id}', { params: { path: { id } } })),
    onSuccess: () => client.invalidateQueries({ queryKey: CHANNELS }),
  })
}

/** Queue a test message; resolves with the job to follow. */
export function useTestChannel() {
  return useMutation({
    mutationFn: (id: number) =>
      unwrap(
        api.POST('/api/v1/admin/notification-channels/{id}/test', { params: { path: { id } } }),
      ),
  })
}

export function useRefreshChannels() {
  const client = useQueryClient()
  return () => client.invalidateQueries({ queryKey: CHANNELS })
}
