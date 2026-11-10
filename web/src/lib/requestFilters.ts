import type { MyRequest } from '@/api/discover'
import type { RequestState } from '@/api/requests'

/*
 * Groups and filters of "Your requests" (spec 0008 slice 6, MEM-20): the member page shows a
 * section per state group (amendment 2026-11-07) and filters by category in the URL
 * (`?category=`, F6); the private page also filters by group. The order puts what the member
 * can act on first: requests still waiting for approval (they can be withdrawn), then those on
 * their way, then the ready ones, then the closed ones; newest change first within a group.
 */

export const REQUEST_GROUPS = ['waiting', 'active', 'ready', 'closed'] as const
export type RequestGroup = (typeof REQUEST_GROUPS)[number]

const GROUP: Record<RequestState, RequestGroup> = {
  pending_approval: 'waiting',
  approved: 'active',
  submitted: 'active',
  in_progress: 'active',
  // An admin has been told; it is not over for the member.
  failed: 'active',
  partially_available: 'ready',
  available: 'ready',
  rejected: 'closed',
  cancelled: 'closed',
  revoked: 'closed',
  removed: 'closed',
}

export function requestGroup(state: RequestState): RequestGroup {
  return GROUP[state]
}

/** Categories of the public requests page; private requests never appear there (Q11). */
export const REQUEST_CATEGORIES = ['movies', 'series', 'music'] as const
export type RequestCategory = (typeof REQUEST_CATEGORIES)[number]

export type RequestFilter = {
  state: RequestGroup | 'all'
  category: RequestCategory | 'all'
}

export type FilterableRequest = Pick<MyRequest, 'id' | 'state' | 'category' | 'updatedAt'>

/** The requests the filter lets through, actionable first, newest change first in a group. */
export function shownRequests<R extends FilterableRequest>(list: R[], filter: RequestFilter): R[] {
  return list
    .filter(
      (r) =>
        (filter.state === 'all' || requestGroup(r.state) === filter.state) &&
        (filter.category === 'all' || r.category === filter.category),
    )
    .sort(
      (a, b) =>
        REQUEST_GROUPS.indexOf(requestGroup(a.state)) -
          REQUEST_GROUPS.indexOf(requestGroup(b.state)) ||
        b.updatedAt.localeCompare(a.updatedAt) ||
        b.id - a.id,
    )
}

/** The categories the member has requests in, for the category chips. */
export function requestCategories(list: Pick<MyRequest, 'category'>[]): RequestCategory[] {
  return REQUEST_CATEGORIES.filter((c) => list.some((r) => r.category === c))
}
