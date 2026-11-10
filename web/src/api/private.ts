import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { ApiError, unwrap } from './errors'
import type { components } from './schema'

export type PrivateArea = components['schemas']['PrivateArea']
export type PrivateList = components['schemas']['PrivateList']
export type PrivateCard = components['schemas']['PrivateCard']
export type PrivatePage = components['schemas']['PrivatePage']
export type PrivateKind = components['schemas']['PrivateKind']
export type PrivateRef = components['schemas']['PrivateRef']
export type PrivateScene = components['schemas']['PrivateScene']
export type PrivateEntity = components['schemas']['PrivateEntity']
export type CreatedPrivateRequests = components['schemas']['CreatedPrivateRequests']
export type PrivateFollow = components['schemas']['PrivateFollow']
export type PrivateFollowKind = components['schemas']['PrivateFollowKind']
export type PrivateSearch = components['schemas']['PrivateSearch']

/** The studio or performer a private search narrows its scenes to (spec 0008 Q7 C). */
export interface SearchFilter {
  kind: 'studio' | 'performer'
  id: string
}

/** Most scenes one request call takes (each is read from the catalog). */
export const MAX_SCENES = 20

/**
 * The private category's own area (spec 0007): every query key starts with `'private'` and is
 * built only by {@link privateKey}, so closing the gate drops all of them at once (spec 0008
 * F2; a test holds this file to it). Nothing is retried on 404: a closed gate stays closed.
 */
export const PRIVATE_PREFIX = 'private'

export function privateKey<const P extends readonly unknown[]>(...parts: P) {
  return [PRIVATE_PREFIX, ...parts] as const
}

const noRetryOn404 = (count: number, error: unknown) =>
  !(error instanceof ApiError && error.status === 404) && count < 2

/** The image of a private title through Starr's proxy (never cached by the browser). */
export function privateImage(ref: string, size: '300' | '600' | '1280'): string {
  return `/api/v1/images/stashdb/${size}/${encodeURIComponent(ref)}`
}

/** The area itself; idle (never asked) while `enabled` is false: the gate is closed. */
export function usePrivateArea(enabled = true) {
  return useQuery({
    queryKey: privateKey('area'),
    queryFn: () => unwrap(api.GET('/api/v1/private')),
    enabled,
    retry: noRetryOn404,
  })
}

export function usePrivateLane(list: PrivateList, enabled: boolean) {
  return useInfiniteQuery({
    queryKey: privateKey('lane', list),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET('/api/v1/private/discover/{list}', {
          params: { path: { list }, query: { page: pageParam } },
        }),
      ),
    initialPageParam: 1,
    getNextPageParam: (last: PrivatePage) => last.next ?? undefined,
    enabled,
    retry: noRetryOn404,
  })
}

/**
 * The area's own search, page by page, optionally narrowed to one studio or performer; idle
 * while `query` is blank. The text travels to the server only in the request body (`POST`),
 * never in a request URL, so access logs keep nothing (the page's own address under `/private`
 * may carry it, spec 0008 F6). Pages after the first only add scenes: the lanes and counts come
 * from the first.
 */
export function usePrivateSearch(query: string, filter: SearchFilter | null = null) {
  const text = query.trim()
  return useInfiniteQuery({
    queryKey: privateKey('search', text, filter?.kind ?? null, filter?.id ?? null),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.POST('/api/v1/private/search', {
          body: {
            query: text,
            ...(pageParam > 1 ? { page: pageParam } : {}),
            ...(filter ? { [filter.kind]: filter.id } : {}),
          },
        }),
      ),
    initialPageParam: 1,
    getNextPageParam: (last: PrivateSearch, pages: PrivateSearch[]) => {
      const loaded = pages.reduce((n, p) => n + p.scenes.length, 0)
      // `next` is about any kind; only more scenes are worth another page.
      return last.next != null && loaded < last.counts.scenes ? last.next : undefined
    },
    enabled: text.length > 0,
    retry: noRetryOn404,
  })
}

export function usePrivateScene(id: string) {
  return useQuery({
    queryKey: privateKey('scene', id),
    queryFn: () =>
      unwrap(api.GET('/api/v1/private/scenes/{sceneId}', { params: { path: { sceneId: id } } })),
    retry: noRetryOn404,
  })
}

export function usePrivateEntity(
  kind: 'performer' | 'studio',
  id: string,
  { enabled = true }: { enabled?: boolean } = {},
) {
  return useQuery({
    enabled,
    queryKey: privateKey(kind, id),
    queryFn: () =>
      kind === 'performer'
        ? unwrap(
            api.GET('/api/v1/private/performers/{performerId}', {
              params: { path: { performerId: id } },
            }),
          )
        : unwrap(
            api.GET('/api/v1/private/studios/{studioId}', {
              params: { path: { studioId: id } },
            }),
          ),
    retry: noRetryOn404,
  })
}

/**
 * The member's requests in the private category (spec 0007 story 7). Polled like the general
 * list; polling does not keep the session's unlock open.
 */
export function usePrivateRequests(showHidden: boolean, enabled = true) {
  return useQuery({
    queryKey: privateKey('requests', { hidden: showHidden }),
    queryFn: () =>
      unwrap(api.GET('/api/v1/private/requests', { params: { query: { hidden: showHidden } } })),
    enabled,
    refetchInterval: 30_000,
    retry: noRetryOn404,
  })
}

/** Request scenes (one request each); statuses, pages and lists refetch afterwards. */
export function useCreatePrivateRequests() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (scenes: string[]) =>
      unwrap(api.POST('/api/v1/private/requests', { body: { scenes } })),
    onSettled: () => client.invalidateQueries({ queryKey: privateKey() }),
  })
}

/**
 * The member's performer and studio follows (spec 0007 slice 5), with the scenes the weekly cap
 * left for them to request. Polling does not keep the unlock open.
 */
export function usePrivateFollows(enabled = true) {
  return useQuery({
    queryKey: privateKey('follows'),
    queryFn: () => unwrap(api.GET('/api/v1/private/follows')),
    enabled,
    refetchInterval: 60_000,
    retry: noRetryOn404,
  })
}

/** Follow a performer or studio; pages and lists refetch afterwards. */
export function useFollowPrivate() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (target: { kind: PrivateFollowKind; id: string }) =>
      unwrap(api.POST('/api/v1/private/follows', { body: target })),
    onSettled: () => client.invalidateQueries({ queryKey: privateKey() }),
  })
}

/** Stop following (requests made so far stay); through the general follow route. */
export function useUnfollowPrivate() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: number) =>
      unwrap(api.DELETE('/api/v1/follows/{id}', { params: { path: { id } } })),
    onSettled: () => client.invalidateQueries({ queryKey: privateKey() }),
  })
}

/**
 * Undo an unfollow (spec 0008 F8, PRIV-17): the follow comes back as it was, with its original
 * cutoff; lists and pages refetch afterwards.
 */
export function useRestorePrivateFollow() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: number) =>
      unwrap(api.POST('/api/v1/private/follows/{id}/restore', { params: { path: { id } } })),
    onSettled: () => client.invalidateQueries({ queryKey: privateKey() }),
  })
}

/** Whether `error` is the closed gate (or a title the catalog does not know). */
export function isNotFound(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404
}
