import { LockIcon, XIcon } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router'

import { useSetPrivateUnlock } from '@/api/privacy'
import { useSession } from '@/auth/session'
import { Button } from '@/components/ui/button'
import { formatTime } from '@/lib/format'
import { gateClosedOf } from '@/lib/privateGate'
import { cn } from '@/lib/utils'

/*
 * The private gate in the shell (spec 0008 IA "Private gate in the navigation", PRIV-04,
 * PRIV-07): a quiet indicator "Private open until 21:14" with "Close" while the session has
 * unlocked the category, a live region that says when it opens or closes, and the neutral
 * notice after the client closed it. Nothing of it is rendered while locked.
 */

/** The session's unlock: `null` while locked or for accounts that may not unlock it. */
function useUnlockedUntil(): string | null {
  const gate = useSession().me?.privateCategory
  return gate?.unlocked ? (gate.unlockedUntil ?? null) : null
}

/**
 * "Private open until 21:14 · Close". `bar` sits beside the search in the header (≥ 1024 px),
 * `strip` is a sticky line under the mobile top bar.
 */
export function PrivateGateIndicator({ layout }: { layout: 'bar' | 'strip' }) {
  const { t } = useTranslation()
  const until = useUnlockedUntil()
  const lock = useSetPrivateUnlock()
  if (until === null) return null
  return (
    <div
      data-private-gate
      className={cn(
        'flex items-center gap-2.5 text-sm whitespace-nowrap text-text-2',
        layout === 'bar'
          ? 'h-10 rounded-full border border-line bg-night-1 pr-1 pl-4'
          : 'sticky top-14 z-30 justify-between border-b border-line bg-night-1 py-1.5 pr-2 pl-4 lg:hidden',
      )}
    >
      <span className="flex items-center gap-2">
        <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-cat-adult" />
        <span>
          <span className="font-semibold text-text">{t('privateGate.label')}</span>{' '}
          {t('privateGate.openUntil')}{' '}
          <time dateTime={until} className="type-data text-text">
            {formatTime(until)}
          </time>
        </span>
      </span>
      <Button
        variant="ghost"
        size="sm"
        disabled={lock.isPending}
        aria-label={t('privateGate.closeLabel')}
        // The indicator goes with the gate: focus moves to the page, never to <body>. Not
        // `mutate`'s callback: that is dropped once this indicator has unmounted (slice 12).
        onClick={() =>
          void lock.mutateAsync(false).then(
            () => document.getElementById('main')?.focus(),
            () => undefined,
          )
        }
        className={layout === 'strip' ? 'h-11' : undefined}
      >
        <LockIcon aria-hidden />
        {t('privateGate.close')}
      </Button>
    </div>
  )
}

/** Says politely when the private area opens or closes (the indicator itself stays quiet). */
export function PrivateGateAnnouncer() {
  const { t } = useTranslation()
  const pending = useSession().pending
  const until = useUnlockedUntil()
  const open = until !== null
  // `null` until the session is known: a reload while unlocked announces nothing.
  const was = useRef<boolean | null>(null)
  const [message, setMessage] = useState('')
  useEffect(() => {
    if (pending) return
    if (was.current === null || was.current === open) {
      was.current = open
      return
    }
    was.current = open
    setMessage(
      until ? t('account.privateUnlocked', { time: formatTime(until) }) : t('privateGate.closed'),
    )
  }, [pending, open, until, t])
  return (
    <p role="status" aria-live="polite" className="sr-only">
      {message}
    </p>
  )
}

/**
 * After the client closed the area itself (expired, closed in another tab, a private 404): a
 * neutral line with "Show again", which unlocks once more. Closing it yourself shows nothing.
 */
export function PrivateGateNotice() {
  const { t } = useTranslation()
  const location = useLocation()
  const navigate = useNavigate()
  const reason = gateClosedOf(location.state)
  const mayUnlock = useSession().me?.privateCategory != null
  const unlock = useSetPrivateUnlock()
  if (reason === null) return null
  // The notice goes away: focus moves to the page, never to <body>.
  const dismiss = () => {
    void navigate(location, { replace: true, state: null })
    document.getElementById('main')?.focus()
  }
  return (
    <div
      role="status"
      // Above the Discover hero, which reaches up under the page's top rows.
      className="relative z-10 mb-6 flex flex-wrap items-center gap-x-4 gap-y-2 rounded-card border border-line bg-night-1 py-3 pr-3 pl-4 text-base text-text-2"
    >
      <LockIcon aria-hidden className="size-4 shrink-0 text-text-3" />
      <p className="min-w-48 flex-1 font-medium text-text">
        {reason === 'idle' ? t('privateGate.notice.idle') : t('privateGate.notice.elsewhere')}
      </p>
      <div className="flex items-center gap-1">
        {mayUnlock && (
          <Button
            variant="ghost"
            size="sm"
            className="max-lg:h-11"
            disabled={unlock.isPending}
            onClick={() => unlock.mutate(true, { onSuccess: dismiss })}
          >
            {t('privateGate.showAgain')}
          </Button>
        )}
        <Button
          variant="ghost"
          size="icon"
          className="max-lg:size-11"
          aria-label={t('privateGate.dismiss')}
          onClick={dismiss}
        >
          <XIcon aria-hidden />
        </Button>
      </div>
    </div>
  )
}
