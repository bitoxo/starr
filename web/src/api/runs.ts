import { useInfiniteQuery, useQuery } from '@tanstack/react-query'

import { api } from './client'
import { unwrap } from './errors'
import type { components, operations } from './schema'

export type RunSummary = components['schemas']['RunSummary']
export type RunDetail = components['schemas']['RunDetail']
export type RunState = components['schemas']['RunState']

/**
 * The newest run of a queued job; polls every second until it finished.
 * `null` while the job has not started yet.
 */
export function useJobRun(jobId: number | null) {
  return useQuery({
    queryKey: ['runs', 'job', jobId],
    enabled: jobId !== null,
    queryFn: async () => {
      const data = await unwrap(
        api.GET('/api/v1/runs', {
          params: { query: { jobId: jobId ?? undefined, limit: 1 } },
        }),
      )
      return data.runs[0] ?? null
    },
    refetchInterval: (query) => {
      const run = query.state.data
      return run && run.state !== 'running' ? false : 1_000
    },
  })
}

export type RunOutcome = NonNullable<
  NonNullable<operations['list_runs']['parameters']['query']>['outcome']
>

export interface RunFilter {
  kind?: string
  outcome?: RunOutcome
}

const PAGE = 50

/** The run history, newest first, one page of 50 at a time ("Load more", ADM-29). */
export function useRuns(filter: RunFilter) {
  return useInfiniteQuery({
    queryKey: ['runs', 'list', filter],
    initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET('/api/v1/runs', {
          params: { query: { ...filter, limit: PAGE, before: pageParam } },
        }),
      ),
    getNextPageParam: (last) => last.nextBefore ?? undefined,
    // Polling refetches every loaded page: only while the first page is all there is.
    refetchInterval: (query) => {
      const pages = query.state.data?.pages ?? []
      if (pages.length > 1) return false
      return pages[0]?.runs.some((r) => r.state === 'running') ? 2_000 : 10_000
    },
  })
}

export function useRun(id: number | null) {
  return useQuery({
    queryKey: ['runs', 'detail', id],
    enabled: id !== null,
    queryFn: async () => {
      const data = await unwrap(
        api.GET('/api/v1/runs/{id}', {
          params: { path: { id: id ?? 0 } },
        }),
      )
      return data
    },
    refetchInterval: (query) => (query.state.data?.state === 'running' ? 1_000 : false),
  })
}
