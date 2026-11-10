import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import type { TitleRef } from './discover'
import { unwrap } from './errors'
import type { components } from './schema'

export type Follow = components['schemas']['Follow']

export const FOLLOWS = ['follows'] as const

/** The logged-in member's follows (new seasons of a series, new albums of an artist), newest first. */
export function useFollows() {
  return useQuery({
    queryKey: FOLLOWS,
    queryFn: () => unwrap(api.GET('/api/v1/follows')),
  })
}

/** The member's follow of the title `ref` names, if any. */
export function followOf(follows: Follow[] | undefined, ref: TitleRef): Follow | undefined {
  return follows?.find((f) => f.kind === ref.kind && f.titleId === ref.id)
}

/**
 * "After season 2": from where on a series' seasons count as new; null before any had an air
 * date, and for artists (their cutoff is `knownThrough`, a day).
 */
export function followCutoff(follow: Follow): number | null {
  return follow.seasonsThrough != null && follow.seasonsThrough > 0 ? follow.seasonsThrough : null
}

function useFollowsMutation<V, R>(call: (vars: V) => Promise<R>) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: call,
    onSettled: () => client.invalidateQueries({ queryKey: FOLLOWS }),
  })
}

export function useFollow() {
  return useFollowsMutation((ref: TitleRef) =>
    unwrap(api.POST('/api/v1/follows', { body: { kind: ref.kind, titleId: ref.id } })),
  )
}

export function useUnfollow() {
  return useFollowsMutation((id: number) =>
    unwrap(api.DELETE('/api/v1/follows/{id}', { params: { path: { id } } })),
  )
}
