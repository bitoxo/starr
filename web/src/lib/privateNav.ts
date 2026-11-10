import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router'

import type { CreatedPrivateRequests, PrivateKind } from '@/api/private'
import { showUndoToast } from '@/lib/undo'
import { privateToast, privateToastId } from '@/lib/privateGate'

/*
 * Paths and feedback of the private area (spec 0008 slice 7). Every path starts with
 * `/private`: nothing private ever lands in another URL (F6). Toasts are neutral (no scene,
 * performer or studio name) and carry the private id prefix, so closing the gate drops them (F2).
 */

export const PRIVATE_REQUESTS = '/private/requests'

export function scenePath(id: string): string {
  return `/private/scenes/${encodeURIComponent(id)}`
}

export function entityPath(kind: Exclude<PrivateKind, 'scene'>, id: string): string {
  return `/private/${kind === 'performer' ? 'performers' : 'studios'}/${encodeURIComponent(id)}`
}

/** The search results page for `query`, optionally narrowed to a studio or performer. */
export function searchPath(
  query: string,
  filter: { kind: 'studio' | 'performer'; id: string } | null = null,
): string {
  const params = new URLSearchParams()
  if (query.trim()) params.set('q', query.trim())
  if (filter) params.set(filter.kind, filter.id)
  const search = params.toString()
  return search ? `/private/search?${search}` : '/private/search'
}

/**
 * "Back" (PRIV-13): one step back in the history while the previous entry is this app's,
 * otherwise to the area's start. Returns the fallback path for the link's `href`.
 */
export function usePrivateBack(): { href: string; back: () => void } {
  const navigate = useNavigate()
  const { key } = useLocation()
  // The first entry of this app in the tab has the key "default": nothing in-app to go back to.
  const inApp = key !== 'default'
  return {
    href: '/private',
    back: () => (inApp ? void navigate(-1) : void navigate('/private')),
  }
}

/**
 * Toasts what a request call did, neutrally (spec 0008 F2, PRIV-21): how many scenes, whether
 * they wait for approval or were approved automatically, what was left out, and the way to the
 * member's private requests.
 */
export function useRequestToast() {
  const { t } = useTranslation('member')
  const navigate = useNavigate()
  return (created: CreatedPrivateRequests) => {
    const count = created.requests.length
    const waiting = created.requests.some((r) => r.state === 'pending_approval')
    const message =
      count === 0
        ? t('private.toasts.nothingNew')
        : waiting
          ? t('private.toasts.requestedWaiting', { count })
          : t('private.toasts.requestedApproved', { count })
    privateToast(message, {
      description:
        created.skipped.length > 0
          ? t('private.skippedToast', { count: created.skipped.length })
          : undefined,
      action: {
        label: t('private.toasts.yourRequests'),
        onClick: () => void navigate(PRIVATE_REQUESTS),
      },
    })
  }
}

/** An undo toast of the private area (neutral text, private id prefix). */
export function privateUndoToast(message: string, onUndo: () => void) {
  return showUndoToast({ id: privateToastId(), message, onUndo })
}
