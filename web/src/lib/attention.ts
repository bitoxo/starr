import type { AdminAttention, AttentionItem, AttentionSection } from '@/api/attention'
import type { Notification } from '@/api/notifications'
import { neutralAcquisitions, neutralHeld, neutralRequest } from '@/lib/privacy'
import { decisionOutcome, type StateKey } from '@/lib/states'

/*
 * The admin attention model in the client (spec 0008 F9): the server's one list drives the
 * Inbox sections, the badge, the heading, `document.title` and the admin bell, so every count
 * is the same number. Helpers here are pure; the Inbox and the shell use them.
 */

export const ATTENTION_KEY = ['admin', 'attention'] as const

/** The Inbox sections, in order. */
export const SECTIONS: AttentionSection[] = ['decide', 'fix', 'paused', 'setup']

/** Neutral private rows (spec 0007 Q9 B) in the shape every admin view shows them. */
export function neutralAttention(data: AdminAttention): AdminAttention {
  return {
    ...data,
    items: data.items.map((item) => ({
      ...item,
      request: item.request ? neutralRequest(item.request) : item.request,
      acquisition: item.acquisition ? neutralAcquisitions([item.acquisition])[0] : item.acquisition,
      writes: item.writes?.held
        ? {
            ...item.writes,
            held: {
              ...item.writes.held,
              preview: neutralHeld(item.writes.held.preview),
              stops: neutralHeld(item.writes.held.stops),
            },
          }
        : item.writes,
    })),
  }
}

/** The items of each section, in the server's order (oldest first). */
export function bySection(items: AttentionItem[]): Record<AttentionSection, AttentionItem[]> {
  const out: Record<AttentionSection, AttentionItem[]> = {
    decide: [],
    fix: [],
    paused: [],
    setup: [],
  }
  for (const item of items) out[item.section].push(item)
  return out
}

/** The attention list without items `ids`, with the counts recomputed (resolved in the client). */
export function withoutItems(data: AdminAttention, ids: readonly string[]): AdminAttention {
  const items = data.items.filter((i) => !ids.includes(i.id))
  if (items.length === data.items.length) return data
  const groups = bySection(items)
  return {
    total: items.length,
    sections: {
      decide: groups.decide.length,
      fix: groups.fix.length,
      paused: groups.paused.length,
      setup: groups.setup.length,
    },
    items,
  }
}

/**
 * Where selection goes after `id` is resolved (spec 0008 Q9 C): the next item in Inbox order,
 * else the one before, else nothing; items in `gone` (resolved together with it) are skipped.
 */
export function nextAfter(
  items: AttentionItem[],
  id: string,
  gone: ReadonlySet<string> = new Set([id]),
): string | null {
  const ordered = SECTIONS.flatMap((s) => items.filter((i) => i.section === s))
  const index = ordered.findIndex((i) => i.id === id)
  if (index < 0) return null
  const stays = (i: AttentionItem) => i.id !== id && !gone.has(i.id)
  return (
    ordered.slice(index + 1).find(stays)?.id ?? ordered.slice(0, index).findLast(stays)?.id ?? null
  )
}

/** The Inbox with item `id` open in its detail panel. */
export function inboxLink(id: string): string {
  return `/admin/inbox?open=${encodeURIComponent(id)}`
}

/** "(3) Inbox – Starr": the admin tab title carries the one count. */
export function titleWithCount(title: string, count: number): string {
  return count > 0 ? `(${count > 99 ? '99+' : count}) ${title}` : title
}

/** The attention item a notification is about, by the item's id (`approval:12`, …). */
export function bellSubject(n: Pick<Notification, 'kind' | 'requestId' | 'acquisitionId'>) {
  switch (n.kind) {
    case 'approval_pending':
      return n.requestId != null ? `approval:${n.requestId}` : null
    case 'acquisition_failed':
      return n.acquisitionId != null ? `failed:${n.acquisitionId}` : null
    case 'import_blocked':
      return n.acquisitionId != null ? `stuck:${n.acquisitionId}` : null
    default:
      return null
  }
}

export type BellEntry = {
  notification: Notification
  /** The attention item it is about, if it maps onto one. */
  subject: string | null
  /** Its subject still needs someone (in the Inbox); null when not known. */
  open: boolean | null
  /** Unread notifications folded into this one (same subject). */
  unread: number
  /** Where a click leads: the Inbox's detail panel while open, else the request or download. */
  link: string
  /** A decided approval: how it turned out (Approved, Declined, Withdrawn, Revoked). */
  outcome: StateKey | null
  /** Read and resolved: shown dimmed under "Earlier". */
  earlier: boolean
}

/** Read entries about resolved subjects leave the bell this long after the notification itself
 * (the decision's time is not in the stream); the Requests and Downloads pages keep them. Entries
 * without a subject (account notifications) are never resolved and stay. */
const EARLIER_DAYS = 7

/**
 * The admin bell (spec 0008 F9, ADM-24): one entry per subject (the newest), with the subject's
 * **current** state from the attention list, linking to its detail panel while it needs someone.
 * `openIds` are the attention items now; without them nothing is marked done. A decided approval
 * names its outcome; once read, resolved entries move under "Earlier" and leave the bell after
 * seven days (slice 12).
 */
export function adminBellEntries(
  list: Notification[],
  openIds: ReadonlySet<string> | null,
  now: Date = new Date(),
): BellEntry[] {
  const entries = new Map<string, BellEntry>()
  for (const n of list) {
    const subject = bellSubject(n)
    // Account notifications name the service and member; folded by both.
    const key = subject ?? `${n.kind}:${n.title}:${n.member ?? ''}:${n.redacted ? n.id : ''}`
    const existing = entries.get(key)
    if (existing) {
      if (!n.read) existing.unread += 1
      continue
    }
    // An approval's own state is the truth; the attention list covers the other kinds.
    const known = n.kind === 'approval_pending' && n.requestState ? n.requestState : null
    const open = known
      ? known === 'pending_approval'
      : subject && openIds
        ? openIds.has(subject)
        : null
    entries.set(key, {
      notification: n,
      subject,
      open,
      unread: n.read ? 0 : 1,
      link: bellLink(n, subject, open),
      outcome: known ? decisionOutcome(known) : null,
      earlier: false,
    })
  }
  const cutoff = now.getTime() - EARLIER_DAYS * 24 * 60 * 60 * 1000
  return [...entries.values()].flatMap((entry) => {
    if (entry.open !== false || entry.unread > 0) return [entry]
    if (new Date(entry.notification.createdAt).getTime() < cutoff) return []
    return [{ ...entry, earlier: true }]
  })
}

function bellLink(n: Notification, subject: string | null, open: boolean | null): string {
  if (subject && open !== false) return inboxLink(subject)
  if (n.kind === 'approval_pending' && n.requestId != null) {
    return `/admin/requests?open=${n.requestId}`
  }
  if (n.acquisitionId != null) return `/admin/downloads?open=${n.acquisitionId}`
  return '/admin/inbox'
}
