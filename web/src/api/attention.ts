import { useQuery, useQueryClient } from '@tanstack/react-query'

import { ATTENTION_KEY, neutralAttention, withoutItems } from '@/lib/attention'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'

export type AdminAttention = components['schemas']['AdminAttention']
export type AttentionItem = components['schemas']['AttentionItem']
export type AttentionKind = components['schemas']['AttentionKind']
export type AttentionSection = components['schemas']['AttentionSection']
export type PausedWrites = components['schemas']['PausedWrites']
export type DecisionResult = components['schemas']['DecisionResult']

/**
 * Everything that needs an admin (spec 0008 F9): the one source for the Inbox, its badge, the
 * section counters, `document.title` and the admin bell. Polls while the tab is visible.
 */
export function useAttention({ enabled = true }: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: ATTENTION_KEY,
    enabled,
    queryFn: () => unwrap(api.GET('/api/v1/admin/attention')),
    select: neutralAttention,
    refetchInterval: 30_000,
  })
}

/**
 * Take resolved items out of the Inbox at once (the counts drop live), then ask the server
 * again.
 */
export function useDropAttentionItems() {
  const client = useQueryClient()
  return (ids: readonly string[]) => {
    const before = client.getQueryData<AdminAttention>(ATTENTION_KEY)
    if (before) client.setQueryData(ATTENTION_KEY, withoutItems(before, ids))
    void client.invalidateQueries({ queryKey: ATTENTION_KEY })
  }
}
