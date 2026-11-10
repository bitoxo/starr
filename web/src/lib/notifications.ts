import type { MyRequest } from '@/api/discover'
import type { Notification, NotificationKind } from '@/api/notifications'
import type { components } from '@/api/schema'
import { titleRefParam } from '@/lib/seasons'
import { notificationState, STATES, type StateTone } from '@/lib/states'

type OpenInLink = components['schemas']['OpenInLink']

/**
 * Where a member's notification leads (spec 0008 slice 6, MEM-21): the request's row on
 * "Your requests", highlighted, with its title open in the detail overlay (`?title=`) when the
 * request names one. Private notifications (listed only while unlocked) stay under `/private`,
 * so nothing private reaches a URL elsewhere.
 */
export function memberNotificationLink(
  n: Pick<Notification, 'kind' | 'category' | 'requestId'>,
  request: Pick<MyRequest, 'kind' | 'titleId'> | null | undefined,
): string {
  if (n.kind === 'account_activate') return '/services'
  const row = n.requestId != null ? `?request=${n.requestId}` : ''
  if (n.category === 'adult') {
    return n.kind === 'follow_capped' ? '/private/following' : `/private/requests${row}`
  }
  const params = new URLSearchParams()
  if (n.requestId != null) params.set('request', String(n.requestId))
  if (request?.titleId != null && request.kind !== 'scene') {
    params.set('title', titleRefParam({ kind: request.kind, id: request.titleId }))
  }
  const query = params.toString()
  return query ? `/requests?${query}` : '/requests'
}

/** "Open in …" for a ready notification: where its request can be watched now, each once. */
export function notificationOpenIn(
  kind: NotificationKind,
  request: { openIn: OpenInLink[]; items: { openIn?: OpenInLink[] }[] } | null | undefined,
): OpenInLink[] {
  if (!request || (kind !== 'request_available' && kind !== 'request_partially_available')) {
    return []
  }
  const links = [...request.openIn, ...request.items.flatMap((i) => i.openIn ?? [])]
  return links.filter((link, i) => links.findIndex((l) => l.url === link.url) === i)
}

/** The state colour of a kind (dot and chip only): request states from the state table. */
export function notificationTone(kind: NotificationKind): StateTone {
  const state = notificationState(kind)
  if (state) return STATES[state].tone
  switch (kind) {
    case 'follow_requested':
      return 'info'
    case 'account_privileged':
      return 'bad'
    // Waiting approvals, capped follows, accounts to activate or locked: needs someone.
    case 'approval_pending':
    case 'follow_capped':
    case 'account_activate':
    case 'account_locked':
      return 'warn'
    // Request states come from the table above.
    case 'request_approved':
    case 'request_rejected':
    case 'request_revoked':
    case 'request_partially_available':
    case 'request_available':
    case 'request_failed':
    case 'acquisition_failed':
    case 'import_blocked':
      return 'neutral'
  }
}

/** "Dark (2017) · S01, S02": the subject as one line. */
export function subjectLabel(n: Pick<Notification, 'title' | 'year' | 'seasons'>): string {
  const head = n.year != null ? `${n.title} (${n.year})` : n.title
  if (n.seasons.length === 0) return head
  const seasons = n.seasons.map((s) => `S${String(s).padStart(2, '0')}`).join(', ')
  return `${head} · ${seasons}`
}

/** The badge text: the count, capped at "9+" so it stays a dot-sized pill. */
export function badgeText(unread: number): string | null {
  if (unread <= 0) return null
  return unread > 9 ? '9+' : String(unread)
}

/** Toggle `kind` in a list of kinds, keeping the given display order. */
export function toggleKind<K extends NotificationKind>(selected: K[], kind: K, order: K[]): K[] {
  const set = new Set(selected)
  if (set.has(kind)) set.delete(kind)
  else set.add(kind)
  return order.filter((k) => set.has(k))
}
