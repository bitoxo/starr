import { describe, expect, it } from 'vitest'

import type { AdminAttention, AttentionItem } from '@/api/attention'
import type { Notification } from '@/api/notifications'

import {
  adminBellEntries,
  bySection,
  inboxLink,
  nextAfter,
  titleWithCount,
  withoutItems,
} from './attention'

function item(id: string, section: AttentionItem['section']): AttentionItem {
  return { id, kind: section === 'decide' ? 'approval' : 'failed', section, redacted: false }
}

const ITEMS = [
  item('approval:1', 'decide'),
  item('approval:2', 'decide'),
  item('failed:5', 'fix'),
  item('setup', 'setup'),
]
const DATA: AdminAttention = {
  total: 4,
  sections: { decide: 2, fix: 1, paused: 0, setup: 1 },
  items: ITEMS,
}

const NOW = new Date('2026-11-07T12:00:00Z')

function note(id: number, kind: Notification['kind'], extra: Partial<Notification>): Notification {
  return {
    id,
    kind,
    createdAt: '2026-11-07T10:00:00Z',
    read: false,
    title: 'Inception',
    category: 'movies',
    seasons: [],
    redacted: false,
    ...extra,
  }
}

describe('the attention model', () => {
  it('groups items by section in Inbox order', () => {
    const groups = bySection(ITEMS)
    expect(groups.decide.map((i) => i.id)).toEqual(['approval:1', 'approval:2'])
    expect(groups.paused).toEqual([])
  })

  it('drops resolved items and keeps every count the same number (ADM-23)', () => {
    const after = withoutItems(DATA, ['approval:1'])
    expect(after.total).toBe(3)
    expect(after.items).toHaveLength(3)
    expect(after.sections).toEqual({ decide: 1, fix: 1, paused: 0, setup: 1 })
    const sum = Object.values(after.sections).reduce((a, b) => a + b, 0)
    expect(sum).toBe(after.total)
    expect(withoutItems(DATA, ['nope'])).toBe(DATA)
    expect(withoutItems(DATA, ['approval:1', 'approval:2']).sections.decide).toBe(0)
  })

  it('moves the selection to the next item, else the previous one (Q9 C)', () => {
    expect(nextAfter(ITEMS, 'approval:2')).toBe('failed:5')
    expect(nextAfter(ITEMS, 'setup')).toBe('failed:5')
    expect(nextAfter([item('only', 'fix')], 'only')).toBeNull()
  })

  it('skips the items resolved together when moving the selection', () => {
    const gone = new Set(['approval:1', 'approval:2'])
    expect(nextAfter(ITEMS, 'approval:1', gone)).toBe('failed:5')
    expect(nextAfter(ITEMS, 'setup', new Set(['setup', 'failed:5']))).toBe('approval:2')
    expect(nextAfter(ITEMS.slice(0, 2), 'approval:1', gone)).toBeNull()
  })

  it('puts the count in the tab title', () => {
    expect(titleWithCount('Inbox – Starr', 3)).toBe('(3) Inbox – Starr')
    expect(titleWithCount('Inbox – Starr', 0)).toBe('Inbox – Starr')
    expect(titleWithCount('Inbox – Starr', 120)).toBe('(99+) Inbox – Starr')
  })
})

describe('the admin bell (ADM-24)', () => {
  it('shows one entry per subject, the newest, with its current state and the panel link', () => {
    const list = [
      note(9, 'acquisition_failed', { acquisitionId: 5 }),
      note(8, 'acquisition_failed', { acquisitionId: 5 }),
      note(7, 'approval_pending', { requestId: 1, member: 'anna' }),
      note(6, 'approval_pending', { requestId: 3, member: 'ben', read: true }),
    ]
    const entries = adminBellEntries(list, new Set(['approval:1', 'failed:5']))
    expect(entries.map((e) => e.notification.id)).toEqual([9, 7, 6])
    expect(entries[0]).toMatchObject({ subject: 'failed:5', open: true, unread: 2 })
    expect(entries[0].link).toBe(inboxLink('failed:5'))
    // Decided meanwhile: no longer "asked for this", it leads to the request.
    expect(entries[2]).toMatchObject({ subject: 'approval:3', open: false, unread: 0 })
    expect(entries[2].link).toBe('/admin/requests?open=3')
  })

  it('does not call anything done before the attention list is known', () => {
    const [entry] = adminBellEntries([note(1, 'import_blocked', { acquisitionId: 4 })], null)
    expect(entry.open).toBeNull()
    expect(entry.link).toBe(inboxLink('stuck:4'))
  })

  it('names how a decided approval turned out instead of "done" (slice 12)', () => {
    const list = [
      note(4, 'approval_pending', { requestId: 4, requestState: 'rejected' }),
      note(3, 'approval_pending', { requestId: 3, requestState: 'revoked' }),
      note(2, 'approval_pending', { requestId: 2, requestState: 'cancelled' }),
      note(1, 'approval_pending', { requestId: 1, requestState: 'in_progress' }),
      note(0, 'approval_pending', { requestId: 9, requestState: 'pending_approval' }),
    ]
    const entries = adminBellEntries(list, new Set(['approval:9']), NOW)
    expect(entries.map((e) => e.outcome)).toEqual([
      'declined',
      'revoked',
      'withdrawn',
      // Whatever happened after the approval, the decision was "approve".
      'approved',
      null,
    ])
  })

  it('moves read, resolved entries under "Earlier" and drops them after 7 days', () => {
    const list = [
      note(5, 'approval_pending', { requestId: 5, member: 'anna' }),
      note(4, 'approval_pending', { requestId: 4, requestState: 'rejected' }),
      note(3, 'approval_pending', { requestId: 3, requestState: 'revoked', read: true }),
      note(2, 'approval_pending', {
        requestId: 2,
        requestState: 'approved',
        read: true,
        createdAt: '2026-10-30T10:00:00Z',
      }),
      note(1, 'approval_pending', { requestId: 1, read: true }),
    ]
    const entries = adminBellEntries(list, new Set(['approval:5', 'approval:1']), NOW)
    // Open ones stay up top even when read; an unread decision too.
    expect(entries.map((e) => [e.notification.id, e.earlier])).toEqual([
      [5, false],
      [4, false],
      [3, true],
      [1, false],
    ])
  })

  it('trusts the request state over the attention list for approvals (review)', () => {
    const decided = note(2, 'approval_pending', { requestId: 2, requestState: 'rejected' })
    const waiting = note(1, 'approval_pending', { requestId: 1, requestState: 'pending_approval' })
    // Attention list not loaded: the outcome is still known.
    expect(adminBellEntries([decided], null, NOW)[0]).toMatchObject({
      open: false,
      outcome: 'declined',
    })
    // A stale attention list that misses a waiting request does not call it done.
    expect(adminBellEntries([waiting], new Set(), NOW)[0]).toMatchObject({
      open: true,
      outcome: null,
      earlier: false,
    })
  })

  it('keeps everything while the attention list is not known', () => {
    const old = note(1, 'approval_pending', {
      requestId: 1,
      read: true,
      createdAt: '2026-09-01T10:00:00Z',
    })
    expect(adminBellEntries([old], null, NOW)[0]).toMatchObject({ earlier: false })
  })

  it('keeps neutral private lines apart (nothing to fold them by)', () => {
    const hidden = { title: '', redacted: true, category: 'adult' }
    const entries = adminBellEntries(
      [note(2, 'account_locked', hidden), note(1, 'account_locked', hidden)],
      new Set(),
    )
    expect(entries).toHaveLength(2)
  })
})
