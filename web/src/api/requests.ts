import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'
import { neutralAcquisitions, neutralDetail, neutralRequests } from '@/lib/privacy'

export type AdminRequest = components['schemas']['AdminRequest']
export type AdminRequestItem = components['schemas']['AdminRequestItem']
export type AdminRequestDetail = components['schemas']['AdminRequestDetail']
export type RequestEvent = components['schemas']['RequestEventDto']
export type RequestState = components['schemas']['RequestState']
export type Acquisition = components['schemas']['AcquisitionDto']
export type AcquisitionState = components['schemas']['AcquisitionState']
export type DownloadStage = components['schemas']['DownloadStage']
export type RequestRef = components['schemas']['RequestRef']
export type CategoryName = components['schemas']['Category']

// Display order; `satisfies` makes a new state from the API fail the
// typecheck until it is listed here (lesson 16).
const REQUEST_STATE_ORDER = {
  pending_approval: true,
  approved: true,
  submitted: true,
  in_progress: true,
  partially_available: true,
  available: true,
  failed: true,
  rejected: true,
  cancelled: true,
  revoked: true,
  removed: true,
} satisfies Record<RequestState, true>
export const REQUEST_STATES = Object.keys(REQUEST_STATE_ORDER) as RequestState[]

const ACQUISITION_STATE_ORDER = {
  pending: true,
  submitted: true,
  downloading: true,
  partially_available: true,
  available: true,
  failed: true,
  cancelled: true,
} satisfies Record<AcquisitionState, true>
export const ACQUISITION_STATES = Object.keys(ACQUISITION_STATE_ORDER) as AcquisitionState[]

const KEY = ['requests'] as const

export type UnlistedAlbum = components['schemas']['UnlistedAlbumDto']

export interface RequestFilter {
  state?: RequestState
  category?: CategoryName
  memberId?: number
}

export function useAdminRequests(filter: RequestFilter) {
  return useQuery({
    queryKey: [...KEY, 'list', filter],
    queryFn: () => unwrap(api.GET('/api/v1/admin/requests', { params: { query: filter } })),
    select: neutralRequests,
    placeholderData: keepPreviousData,
    refetchInterval: 15_000,
  })
}

export function useAdminRequest(id: number | null) {
  return useQuery({
    queryKey: [...KEY, 'detail', id],
    enabled: id !== null,
    queryFn: () =>
      unwrap(api.GET('/api/v1/admin/requests/{id}', { params: { path: { id: id ?? 0 } } })),
    select: neutralDetail,
  })
}

export function useAcquisitions(state?: AcquisitionState) {
  return useQuery({
    queryKey: [...KEY, 'acquisitions', state ?? null],
    queryFn: () => unwrap(api.GET('/api/v1/admin/acquisitions', { params: { query: { state } } })),
    select: neutralAcquisitions,
    placeholderData: keepPreviousData,
    refetchInterval: 15_000,
  })
}

/** Every decision changes the inbox, the lists and the acquisitions. */
function useRequestsMutation<V, R>(call: (vars: V) => Promise<R>) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: call,
    onSettled: () => client.invalidateQueries({ queryKey: KEY }),
  })
}

export function useApproveRequests() {
  return useRequestsMutation((requests: RequestRef[]) =>
    unwrap(api.POST('/api/v1/admin/approvals/approve', { body: { requests } })),
  )
}

export function useRejectRequests() {
  return useRequestsMutation(({ requests, reason }: { requests: RequestRef[]; reason: string }) =>
    unwrap(api.POST('/api/v1/admin/approvals/reject', { body: { requests, reason } })),
  )
}

/** Revoke requests with a reason the members read (spec 0004 R3). */
export function useRevokeRequests() {
  return useRequestsMutation(({ requests, reason }: { requests: RequestRef[]; reason: string }) =>
    unwrap(api.POST('/api/v1/admin/requests/revoke', { body: { requests, reason } })),
  )
}

export function useRetryAcquisition() {
  return useRequestsMutation((id: number) =>
    unwrap(api.POST('/api/v1/admin/acquisitions/{id}/retry', { params: { path: { id } } })),
  )
}

/**
 * What a request's units are, in one line: "S01, S03" for seasons, the album titles for
 * music, or null for a movie.
 */
export function unitsLabel(
  units: { season?: number | null; album?: { title: string } | null }[],
): string | null {
  const albums = units.flatMap((u) => (u.album ? [u.album.title] : []))
  if (albums.length > 0) return albums.join(', ')
  return seasonLabel(units.map((u) => u.season))
}

/** "S01, S03" for the seasons of `items`, or null for a movie. */
export function seasonLabel(seasons: (number | null | undefined)[]): string | null {
  const numbers = seasons.filter((n): n is number => typeof n === 'number')
  if (numbers.length === 0) return null
  return numbers.map((n) => `S${String(n).padStart(2, '0')}`).join(', ')
}

/** The most a reason may hold (the server counts code points). */
export const REASON_MAX = 500

/** A request named with its owner, as every admin decision names it (lesson 4). */
export function refOf(request: AdminRequest): RequestRef {
  return { id: request.id, memberId: request.member.id }
}
