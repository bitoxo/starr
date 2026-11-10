import { type QueryClient, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef } from 'react'
import { useLocation, useNavigate } from 'react-router'
import { type ExternalToast, toast } from 'sonner'

import { ApiError } from '@/api/errors'
import type { NotificationPage } from '@/api/notifications'
import { PRIVATE_PREFIX, privateKey } from '@/api/private'
import { ME_KEY, useSession } from '@/auth/session'
import { ATTENTION_KEY } from '@/lib/attention'

/*
 * The private gate in the client (spec 0008 F2, ADR 0015). The server decides (ADR 0014); the
 * client follows `privateCategory { unlocked, unlockedUntil }` of `/api/v1/auth/me` and, the
 * moment it reads "closed", forgets everything private it holds: every query under the
 * `['private', …]` key, private notification entries and every toast whose id starts with
 * `private:`. It reads `me` again when the unlock time passes, on focus and visibility changes,
 * every minute while unlocked, after any private 404, and when another tab changed the gate.
 */

export const PRIVATE_TOAST_PREFIX = 'private:'
let toastSeq = 0

/** A fresh toast id with the private prefix (for custom toasts such as undo). */
export function privateToastId(): string {
  toastSeq += 1
  return `${PRIVATE_TOAST_PREFIX}${toastSeq}`
}

/**
 * A toast about something in the private area. The text must be neutral (no scene, performer or
 * studio name, spec 0008 F2); its id carries the private prefix, so closing the gate dismisses it.
 */
export function privateToast(
  message: string,
  options: Omit<ExternalToast, 'id'> = {},
  tone: 'info' | 'error' = 'info',
): string {
  const id = privateToastId()
  if (tone === 'error') toast.error(message, { ...options, id })
  else toast(message, { ...options, id })
  return id
}

/** Dismiss every toast whose id starts with `private:`; others stay. */
export function dismissPrivateToasts(): void {
  for (const shown of toast.getToasts()) {
    if (typeof shown.id === 'string' && shown.id.startsWith(PRIVATE_TOAST_PREFIX)) {
      toast.dismiss(shown.id)
    }
  }
}

/** Forget everything private the client holds: queries, notification entries, toasts. */
export function dropPrivateState(client: QueryClient): void {
  client.removeQueries({ queryKey: privateKey() })
  // The admin Inbox names private titles while unlocked (spec 0008 F9): reset, so a mounted
  // Inbox drops them at once instead of showing them until the answer comes, and asked again.
  void client.resetQueries({ queryKey: ATTENTION_KEY })
  // Notification pages are shared with other categories: drop the private entries now, the
  // next answer of the server leaves them out (spec 0008 Q24 A).
  const withoutPrivate = (page: NotificationPage) => ({
    ...page,
    items: page.items.filter((n) => n.category !== 'adult'),
  })
  client.setQueriesData({ queryKey: ['notifications'] }, (data: unknown) => {
    if (isNotificationPage(data)) return withoutPrivate(data)
    // The paged full list (profile): every page.
    if (isPaged(data)) return { ...data, pages: data.pages.map(withoutPrivate) }
    return data
  })
  // Everything else may hold private rows the gate decided over (an admin's request lists with
  // titles): fetched again, now behind the closed gate.
  void client.invalidateQueries()
  dismissPrivateToasts()
}

function isNotificationPage(data: unknown): data is NotificationPage {
  return typeof data === 'object' && data !== null && 'items' in data && 'unread' in data
}

function isPaged(data: unknown): data is { pages: NotificationPage[] } {
  return (
    typeof data === 'object' &&
    data !== null &&
    'pages' in data &&
    Array.isArray(data.pages) &&
    data.pages.every(isNotificationPage)
  )
}

const CHANNEL = 'starr:private-gate'

/** Tell the other tabs of this browser that the gate changed; they read `me` again. */
export function announceGateChange(): void {
  if (typeof BroadcastChannel === 'undefined') return
  const channel = new BroadcastChannel(CHANNEL)
  channel.postMessage('changed')
  channel.close()
}

/** Why the client closed the area: the idle time ran out, or it was closed elsewhere. */
export type GateClosed = 'idle' | 'elsewhere'

/** Navigation state of the page the client returns to after closing the area itself. */
export interface GateClosedState {
  privateGateClosed: GateClosed
}

export function gateClosedOf(state: unknown): GateClosed | null {
  if (typeof state !== 'object' || state === null || !('privateGateClosed' in state)) return null
  const reason = state.privateGateClosed
  return reason === 'idle' || reason === 'elsewhere' ? reason : null
}

// Set while this tab locks on the user's behalf: that close shows no notice.
let closingByUser = false

/** The next close of the gate is the user's own ("Close", the menu): no notice. */
export function expectUserClose(expected: boolean): void {
  closingByUser = expected
}

const MINUTE = 60_000
// Server and browser clocks differ a little: a close this close to the unlock time is idle.
const IDLE_SLACK = 5_000

function isPrivateNotFound(error: unknown, queryKey?: readonly unknown[]): boolean {
  if (!(error instanceof ApiError) || error.status !== 404) return false
  return error.code === 'private.not_found' || queryKey?.[0] === PRIVATE_PREFIX
}

/**
 * Keep the client's private state in line with the server's gate. Mount once per shell; `home`
 * is where the shell returns to when the client closes the area.
 */
export function usePrivateGate(home: string): void {
  const client = useQueryClient()
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const path = useRef(pathname)
  useEffect(() => {
    path.current = pathname
  }, [pathname])
  const me = useSession().me
  const unlocked = me?.privateCategory?.unlocked === true
  const until = unlocked ? (me?.privateCategory?.unlockedUntil ?? null) : null
  const who = me ? `${me.kind}:${me.username}` : null
  const last = useRef({ who, unlocked, until })

  // Open → closed for the same account: forget, then leave (with a notice unless the user
  // closed it).
  useEffect(() => {
    const before = last.current
    last.current = { who, unlocked, until }
    if (!before.unlocked || unlocked) return
    // The session ended (expired, signed out in another tab) or another account took over:
    // forget, without a notice; the guards move to the login or the new account's home.
    if (who === null || before.who !== who) {
      dropPrivateState(client)
      return
    }
    const byUser = closingByUser
    closingByUser = false
    dropPrivateState(client)
    if (byUser) {
      if (path.current.startsWith('/private')) void navigate(home, { replace: true })
      return
    }
    const idle = before.until !== null && Date.parse(before.until) <= Date.now() + IDLE_SLACK
    const state: GateClosedState = { privateGateClosed: idle ? 'idle' : 'elsewhere' }
    void navigate(home, { replace: true, state })
  }, [client, navigate, home, who, unlocked, until])

  // Read `me` again when the unlock time passes.
  useEffect(() => {
    if (until === null) return
    const wait = Math.max(Date.parse(until) - Date.now() + 1_000, 0)
    const timer = setTimeout(() => void client.invalidateQueries({ queryKey: ME_KEY }), wait)
    return () => clearTimeout(timer)
  }, [client, until])

  // While unlocked: every minute the tab is visible, and on focus or becoming visible.
  useEffect(() => {
    if (!unlocked) return
    const recheck = () => void client.invalidateQueries({ queryKey: ME_KEY })
    const whenVisible = () => {
      if (document.visibilityState === 'visible') recheck()
    }
    const interval = setInterval(whenVisible, MINUTE)
    window.addEventListener('focus', recheck)
    document.addEventListener('visibilitychange', whenVisible)
    return () => {
      clearInterval(interval)
      window.removeEventListener('focus', recheck)
      document.removeEventListener('visibilitychange', whenVisible)
    }
  }, [client, unlocked])

  // Another tab locked or unlocked; any private 404.
  useEffect(() => {
    const recheck = () => void client.invalidateQueries({ queryKey: ME_KEY })
    const channel = typeof BroadcastChannel === 'undefined' ? null : new BroadcastChannel(CHANNEL)
    if (channel) channel.onmessage = recheck
    const queries = client.getQueryCache().subscribe((event) => {
      if (event.type !== 'updated' || event.action.type !== 'error') return
      if (isPrivateNotFound(event.action.error, event.query.queryKey)) recheck()
    })
    const mutations = client.getMutationCache().subscribe((event) => {
      if (event.type !== 'updated' || event.mutation.state.status !== 'error') return
      if (isPrivateNotFound(event.mutation.state.error)) recheck()
    })
    return () => {
      channel?.close()
      queries()
      mutations()
    }
  }, [client])
}
