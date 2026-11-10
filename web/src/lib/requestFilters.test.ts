import { describe, expect, it } from 'vitest'

import {
  REQUEST_GROUPS,
  requestCategories,
  requestGroup,
  shownRequests,
  type FilterableRequest,
} from './requestFilters'

function req(
  id: number,
  state: FilterableRequest['state'],
  patch: Partial<FilterableRequest> = {},
): FilterableRequest {
  return { id, state, category: 'movies', updatedAt: `2026-11-0${id}T12:00:00Z`, ...patch }
}

describe('request filters', () => {
  it('groups every request state', () => {
    expect(requestGroup('pending_approval')).toBe('waiting')
    for (const s of ['approved', 'submitted', 'in_progress', 'failed'] as const) {
      expect(requestGroup(s)).toBe('active')
    }
    expect(requestGroup('partially_available')).toBe('ready')
    expect(requestGroup('available')).toBe('ready')
    for (const s of ['rejected', 'cancelled', 'revoked', 'removed'] as const) {
      expect(requestGroup(s)).toBe('closed')
    }
    expect(REQUEST_GROUPS).toEqual(['waiting', 'active', 'ready', 'closed'])
  })

  it('puts what the member can act on first, newest first within a group', () => {
    const list = [
      req(1, 'available'),
      req(2, 'rejected'),
      req(3, 'in_progress'),
      req(4, 'pending_approval'),
      req(5, 'pending_approval'),
      req(6, 'available'),
    ]
    expect(shownRequests(list, { state: 'all', category: 'all' }).map((r) => r.id)).toEqual([
      5, 4, 3, 6, 1, 2,
    ])
  })

  it('filters by state group and category', () => {
    const list = [
      req(1, 'available', { category: 'series' }),
      req(2, 'available'),
      req(3, 'pending_approval', { category: 'series' }),
    ]
    expect(shownRequests(list, { state: 'ready', category: 'all' }).map((r) => r.id)).toEqual([
      2, 1,
    ])
    expect(shownRequests(list, { state: 'all', category: 'series' }).map((r) => r.id)).toEqual([
      3, 1,
    ])
    expect(shownRequests(list, { state: 'ready', category: 'series' }).map((r) => r.id)).toEqual([
      1,
    ])
  })

  it('offers the categories the member has requests in, in a fixed order', () => {
    const list = [req(1, 'available', { category: 'music' }), req(2, 'available')]
    expect(requestCategories(list)).toEqual(['movies', 'music'])
  })
})
