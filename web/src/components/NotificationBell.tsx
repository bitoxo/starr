import { BellIcon, CheckCheckIcon, ExternalLinkIcon } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate } from 'react-router'

import { useAttention } from '@/api/attention'
import type { CategoryName } from '@/api/categories'
import { type MyRequest, useMyRequests } from '@/api/discover'
import {
  type Notification,
  type Scope,
  useMarkRead,
  useNotificationHistory,
  useNotifications,
} from '@/api/notifications'
import { Cover } from '@/components/Poster'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { StateChip } from '@/components/StateChip'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { adminBellEntries, bellSubject, type BellEntry } from '@/lib/attention'
import { formatRelative } from '@/lib/format'
import {
  badgeText,
  memberNotificationLink,
  notificationOpenIn,
  notificationTone,
  subjectLabel,
} from '@/lib/notifications'
import { focusElement } from '@/lib/returnFocus'
import { cn } from '@/lib/utils'

const TONE_DOT = {
  ok: 'bg-ok',
  info: 'bg-info',
  warn: 'bg-warn',
  bad: 'bg-bad',
  neutral: 'bg-text-3',
} as const

/**
 * The bell with the unread count; opens the stream as a popover. `icon` is the round button of
 * the top bar, `row` the labelled sidebar item, `rail` its icon-only form with a tooltip. The
 * admin bell counts what needs an admin, the same number as the Inbox (spec 0008 F9, ADM-23).
 */
export function NotificationBell({
  scope,
  layout = 'icon',
}: {
  scope: Scope
  layout?: 'icon' | 'row' | 'rail'
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const stream = useNotifications(scope)
  const attention = useAttention({ enabled: scope === 'admin' })
  const count = scope === 'admin' ? (attention.data?.total ?? 0) : (stream.data?.unread ?? 0)
  const badge = badgeText(count)
  const label =
    count === 0
      ? t('notifications.bell')
      : scope === 'admin'
        ? t('notifications.bellAttention', { count })
        : t('notifications.bellUnread', { count })

  const trigger = (
    <PopoverTrigger
      className={cn(
        'relative shrink-0 cursor-pointer text-text-2 transition-colors hover:bg-night-2 hover:text-text data-[state=open]:bg-night-2 data-[state=open]:text-text',
        layout === 'icon' && 'grid size-11 place-items-center rounded-full',
        layout === 'row' && 'flex h-10 w-full items-center gap-3 rounded-card px-3 text-base',
        layout === 'rail' && 'grid h-10 w-full place-items-center rounded-card',
      )}
      aria-label={label}
    >
      <span className="relative grid place-items-center">
        <BellIcon
          aria-hidden
          className={layout === 'icon' ? 'size-5' : 'size-4'}
          strokeWidth={1.75}
        />
        {badge && layout !== 'row' && (
          <span
            aria-hidden
            className={cn(
              'chip type-data absolute rounded-full text-center font-semibold [--chip:var(--color-warn)]',
              layout === 'icon'
                ? '-top-2.5 -right-3 min-w-5 px-1 text-[11px] leading-5'
                : '-top-2.5 left-2.5 min-w-4 px-1 text-[10px] leading-4',
            )}
          >
            {badge}
          </span>
        )}
      </span>
      {layout === 'row' && (
        <>
          <span aria-hidden className="flex-1 text-left">
            {t('notifications.bell')}
          </span>
          {badge && (
            <span
              aria-hidden
              className="chip type-data rounded-full px-2 text-xs font-medium [--chip:var(--color-warn)]"
            >
              {badge}
            </span>
          )}
        </>
      )}
    </PopoverTrigger>
  )

  return (
    <Popover open={open} onOpenChange={setOpen}>
      {layout === 'rail' ? (
        <Tooltip>
          <TooltipTrigger asChild>{trigger}</TooltipTrigger>
          <TooltipContent side="right" sideOffset={10}>
            {t('notifications.bell')}
          </TooltipContent>
        </Tooltip>
      ) : (
        trigger
      )}
      <PopoverContent
        // Members: the list focuses its first notification itself (MEM-21).
        onOpenAutoFocus={scope === 'member' ? (event) => event.preventDefault() : undefined}
        // An opened entry may have moved focus on (the highlighted request row, the detail
        // overlay): keep it there; else back to the bell.
        onCloseAutoFocus={(event) => {
          const active = document.activeElement
          if (active && active !== document.body && active.isConnected) event.preventDefault()
        }}
        side={layout === 'icon' ? 'bottom' : 'right'}
        className="flex max-h-[min(32rem,80dvh)] w-[min(24rem,calc(100vw-2rem))] flex-col p-0"
      >
        <NotificationList scope={scope} onNavigate={() => setOpen(false)} />
      </PopoverContent>
    </Popover>
  )
}

/** The stream: newest first, unread ones marked, "mark all read" on top. */
export function NotificationList({ scope, onNavigate }: { scope: Scope; onNavigate: () => void }) {
  if (scope === 'member') return <MemberBellList onNavigate={onNavigate} />
  return <AdminBellList onNavigate={onNavigate} />
}

/** The admin bell: one entry per subject with its current state (ADM-24). */
function AdminBellList({ onNavigate }: { onNavigate: () => void }) {
  const scope = 'admin'
  const { t } = useTranslation()
  const stream = useNotifications(scope)
  const markRead = useMarkRead(scope)
  const navigate = useNavigate()
  const attention = useAttention({ enabled: true })
  const items = stream.data?.items ?? []
  const unread = stream.data?.unread ?? 0
  const openIds = attention.data ? new Set(attention.data.items.map((i) => i.id)) : null
  // Admins: one entry per subject with its current state, linking to the Inbox's panel (ADM-24).
  const entries: BellEntry[] = adminBellEntries(items, openIds)
  // Read entries about resolved subjects sit dimmed below the rest (slice 12).
  const current = entries.filter((e) => !e.earlier)
  const earlier = entries.filter((e) => e.earlier)
  const earlierId = useId()

  const open = (entry: BellEntry) => {
    const n = entry.notification
    const folded = entry.subject ? sameSubject(items, entry) : [n]
    const ids = folded.filter((x) => !x.read).map((x) => x.id)
    if (ids.length > 0) markRead.mutate(ids)
    onNavigate()
    void navigate(entry.link)
  }

  return (
    <>
      <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-3">
        <h2 className="type-display text-lg font-bold text-text">{t('notifications.title')}</h2>
        <Button variant="ghost" size="sm" {...markAllProps(unread, markRead)}>
          <CheckCheckIcon aria-hidden />
          {t('notifications.markAllRead')}
        </Button>
      </div>
      {attention.data && (
        <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-2.5 text-sm">
          <span className="text-text-2">
            {attention.data.total > 0
              ? t('notifications.needYou', { count: attention.data.total })
              : t('notifications.nothingNeedsYou')}
          </span>
          <Link
            to="/admin/inbox"
            onClick={onNavigate}
            className="font-medium text-text underline-offset-4 hover:underline"
          >
            {t('notifications.openInbox')}
          </Link>
        </div>
      )}
      {stream.isError && (
        <p className="px-4 py-6 text-sm text-text-2">{t('notifications.loadFailed')}</p>
      )}
      {stream.data && entries.length === 0 && (
        <div className="flex flex-col gap-1 px-4 py-8">
          <p className="font-medium text-text">{t('notifications.emptyTitle')}</p>
          <p className="text-sm text-text-2">{t('notifications.emptyAdmin')}</p>
        </div>
      )}
      {entries.length > 0 && (
        <div className="flex-1 overflow-y-auto py-1">
          {current.length > 0 && (
            <ul>
              {current.map((entry) => (
                <AdminBellItem key={entry.notification.id} entry={entry} onOpen={open} />
              ))}
            </ul>
          )}
          {earlier.length > 0 && (
            <>
              <h3
                id={earlierId}
                className="type-data px-4 pt-3 pb-1 text-xs tracking-wide text-text-3 uppercase"
              >
                {t('notifications.earlier')}
              </h3>
              <ul aria-labelledby={earlierId}>
                {earlier.map((entry) => (
                  <AdminBellItem key={entry.notification.id} entry={entry} onOpen={open} />
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </>
  )
}

/** One admin bell entry: what happened, the subject, and its state now (ADM-24, slice 12). */
function AdminBellItem({ entry, onOpen }: { entry: BellEntry; onOpen: (e: BellEntry) => void }) {
  const { t } = useTranslation()
  const n = entry.notification
  return (
    <li className={cn(entry.earlier && 'opacity-70')}>
      <button
        type="button"
        onClick={() => onOpen(entry)}
        className="flex w-full cursor-pointer gap-3 px-4 py-3 text-left transition-colors hover:bg-night-1"
      >
        <span
          aria-hidden
          className={cn(
            'mt-2 size-2 shrink-0 rounded-full',
            TONE_DOT[notificationTone(n.kind)],
            n.read && 'opacity-40',
          )}
        />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className={cn('text-sm', n.read ? 'text-text-2' : 'font-medium text-text')}>
            {t(`notifications.kind.${n.kind}`, {
              // A neutral private line names no member (spec 0007 Q9).
              member: n.member || t('privacy.member'),
              // Music is listened to ("Ready to listen to").
              context:
                n.category === 'music' ? 'music' : n.category === 'adult' ? 'adult' : undefined,
            })}
            {!n.read && <span className="sr-only"> · {t('notifications.unread')}</span>}
          </span>
          <span className="truncate text-sm text-text">
            {/* A private notification behind a closed gate (spec 0007 Q8, Q9). */}
            {n.redacted ? t('privacy.title') : subjectLabel(n)}
          </span>
          {n.reason && (
            <span className="line-clamp-2 text-sm text-text-2">
              {t('notifications.reason', { reason: n.reason })}
            </span>
          )}
          <span className="flex flex-wrap items-center gap-2">
            {entry.outcome ? (
              // How a decided approval turned out, from the one state table (F1).
              <StateChip state={{ key: entry.outcome }} className="px-2 py-0 text-xs" />
            ) : (
              entry.open !== null && (
                <StatusChip tone={entry.open ? 'warn' : 'neutral'} className="px-2 py-0 text-xs">
                  {entry.open ? t('notifications.current.open') : t('notifications.current.done')}
                </StatusChip>
              )
            )}
            <span className="type-data text-xs text-text-3">{formatRelative(n.createdAt)}</span>
            {entry.unread > 1 && (
              <span className="text-xs text-text-3">
                {t('notifications.repeated', { count: entry.unread - 1 })}
              </span>
            )}
          </span>
        </span>
      </button>
    </li>
  )
}

/** The notifications folded into `entry` (same subject), to mark them read together. */
function sameSubject(items: Notification[], entry: BellEntry): Notification[] {
  return items.filter((n) => bellSubject(n) === entry.subject)
}

/** The member's requests by id: titles to open, covers and "Open in" for notifications. */
function useRequestsById(): Map<number, MyRequest> {
  // Hidden ones too: a notification may be about a request the member hid since.
  const requests = useMyRequests(true)
  return new Map((requests.data ?? []).map((r) => [r.id, r]))
}

/**
 * One member notification (spec 0008 slice 6, MEM-21): the whole row is one link to the title
 * over its highlighted request row (or the row only, or My services), with the request's cover;
 * ready ones offer "Open in …" for each server it is in.
 */
function MemberNotificationItem({
  n,
  request,
  onOpen,
}: {
  n: Notification
  request: MyRequest | undefined
  onOpen: (n: Notification) => void
}) {
  const { t } = useTranslation(['common', 'member'])
  const openIn = notificationOpenIn(n.kind, request)
  return (
    <li className="relative flex gap-3 px-4 py-3 transition-colors hover:bg-night-1">
      {request ? (
        <span aria-hidden className="w-9 shrink-0 self-start">
          <Cover
            file={request.poster}
            title={request.title}
            sizes="36px"
            kind={request.kind === 'artist' ? 'album' : request.kind}
            category={n.category as CategoryName}
          />
        </span>
      ) : (
        <span aria-hidden className="grid w-9 shrink-0 justify-center pt-2">
          <span className={cn('size-2 rounded-full', TONE_DOT[notificationTone(n.kind)])} />
        </span>
      )}
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <Link
          data-notification
          to={memberNotificationLink(n, request)}
          onClick={() => onOpen(n)}
          // The link covers the row; "Open in" sits above it.
          className="flex flex-col gap-0.5 rounded-control after:absolute after:inset-0"
        >
          <span
            className={cn(
              'flex items-center gap-2 text-sm',
              n.read ? 'text-text-2' : 'font-medium text-text',
            )}
          >
            {request && (
              <span
                aria-hidden
                className={cn(
                  'size-2 shrink-0 rounded-full',
                  TONE_DOT[notificationTone(n.kind)],
                  n.read && 'opacity-40',
                )}
              />
            )}
            {t(`notifications.kind.${n.kind}`, {
              member: n.member || t('privacy.member'),
              context:
                n.category === 'music' ? 'music' : n.category === 'adult' ? 'adult' : undefined,
            })}
            {!n.read && <span className="sr-only"> · {t('notifications.unread')}</span>}
          </span>
          <span className="truncate text-sm text-text">{subjectLabel(n)}</span>
        </Link>
        {n.reason && (
          <span className="line-clamp-2 text-sm text-text-2">
            {t('notifications.reason', { reason: n.reason })}
          </span>
        )}
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="type-data text-xs text-text-3">{formatRelative(n.createdAt)}</span>
          {openIn.map((link) => (
            <a
              key={link.url}
              href={link.url}
              target="_blank"
              rel="noreferrer"
              className="relative z-10 inline-flex min-h-6 items-center gap-1 text-xs font-medium text-text underline-offset-4 hover:underline"
            >
              {t('member:title.openIn', { service: link.name })}
              <ExternalLinkIcon aria-hidden className="size-3 text-text-3" />
              <span className="sr-only">{t('notifications.newTab')}</span>
            </a>
          ))}
        </span>
      </span>
    </li>
  )
}

/**
 * "Mark all read" without `disabled`: a disabled button would drop focus to `<body>` right
 * after it was pressed, so it says `aria-disabled` and ignores the click instead.
 */
function markAllProps(unread: number, markRead: ReturnType<typeof useMarkRead>) {
  const off = unread === 0 || markRead.isPending
  return {
    'aria-disabled': off || undefined,
    className: 'aria-disabled:cursor-default aria-disabled:opacity-50',
    onClick: () => {
      if (!off) markRead.mutate(undefined)
    },
  }
}

/** Marks a notification read when it is opened. */
function useOpenNotification() {
  const markRead = useMarkRead('member')
  return { markRead, open: (n: Notification) => !n.read && markRead.mutate([n.id]) }
}

/** The bell's list for members: newest first, focus on the first one, "See all" below. */
function MemberBellList({ onNavigate }: { onNavigate: () => void }) {
  const { t } = useTranslation()
  const stream = useNotifications('member')
  const byId = useRequestsById()
  const { markRead, open } = useOpenNotification()
  const list = useRef<HTMLDivElement>(null)
  const heading = useRef<HTMLHeadingElement>(null)
  const items = stream.data?.items ?? []
  const unread = stream.data?.unread ?? 0
  const focused = useRef(false)

  // Focus the first notification once there is one; until then the heading, so focus stays in
  // the popover (MEM-21).
  useEffect(() => {
    if (focused.current) return
    const first = list.current?.querySelector<HTMLElement>('a[data-notification]')
    if (first) {
      focused.current = true
      first.focus()
    } else if (heading.current && !list.current?.contains(document.activeElement)) {
      focusElement(heading.current)
      if (stream.data) focused.current = true
    }
  }, [items.length, stream.data])

  return (
    <div ref={list} className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-3">
        <h2 ref={heading} className="type-display text-lg font-bold text-text outline-none">
          {t('notifications.title')}
        </h2>
        <Button variant="ghost" size="sm" {...markAllProps(unread, markRead)}>
          <CheckCheckIcon aria-hidden />
          {t('notifications.markAllRead')}
        </Button>
      </div>
      {stream.isError && (
        <p className="px-4 py-6 text-sm text-text-2">{t('notifications.loadFailed')}</p>
      )}
      {stream.data && items.length === 0 && (
        <div className="flex flex-col gap-1 px-4 py-8">
          <p className="font-medium text-text">{t('notifications.emptyTitle')}</p>
          <p className="text-sm text-text-2">{t('notifications.emptyMember')}</p>
        </div>
      )}
      {items.length > 0 && (
        <ul className="flex-1 overflow-y-auto py-1">
          {items.map((n) => (
            <MemberNotificationItem
              key={n.id}
              n={n}
              request={n.requestId != null ? byId.get(n.requestId) : undefined}
              onOpen={(x) => {
                open(x)
                onNavigate()
              }}
            />
          ))}
        </ul>
      )}
      <div className="border-t border-line px-4 py-2.5 text-sm">
        <Link
          to="/profile/notifications"
          onClick={onNavigate}
          className="inline-flex min-h-6 items-center font-medium text-text underline-offset-4 hover:underline"
        >
          {t('notifications.seeAll')}
        </Link>
      </div>
    </div>
  )
}

/**
 * `/profile/notifications`: the member's whole stream, page by page (MEM-21). "Show older"
 * loads the next page and moves focus to its first notification.
 */
export function NotificationHistory() {
  const { t } = useTranslation()
  const history = useNotificationHistory()
  const byId = useRequestsById()
  const { markRead, open } = useOpenNotification()
  const list = useRef<HTMLUListElement>(null)
  const [focusIndex, setFocusIndex] = useState<number | null>(null)
  const items = history.data?.pages.flatMap((p) => p.items) ?? []
  const unread = history.data?.pages[0]?.unread ?? 0

  useEffect(() => {
    if (focusIndex == null) return
    const target = list.current?.querySelectorAll<HTMLElement>('a[data-notification]')[focusIndex]
    if (target) {
      target.focus()
      setFocusIndex(null)
    }
  }, [focusIndex, items.length])

  if (history.isPending) {
    return (
      <ul aria-busy aria-label={t('status.loading')} className="flex flex-col gap-3">
        {[0, 1, 2].map((i) => (
          <li key={i} className="flex gap-3">
            <Skeleton className="aspect-2/3 w-9 shrink-0 rounded-card" />
            <span className="flex flex-1 flex-col gap-2">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-4 w-56 max-w-full" />
            </span>
          </li>
        ))}
      </ul>
    )
  }
  // A failed first load replaces the list; a failed "Show older" keeps what is there.
  if (history.isError && !history.data) {
    return (
      <QueryError message={t('notifications.loadFailed')} onRetry={() => void history.refetch()} />
    )
  }
  return (
    <div className="flex flex-col gap-3">
      {items.length === 0 ? (
        <div className="flex flex-col gap-1 rounded-card border border-line px-4 py-6">
          <p className="font-medium text-text">{t('notifications.emptyTitle')}</p>
          <p className="text-sm text-text-2">{t('notifications.emptyMember')}</p>
        </div>
      ) : (
        <>
          <div className="flex justify-end">
            <Button variant="ghost" size="sm" {...markAllProps(unread, markRead)}>
              <CheckCheckIcon aria-hidden />
              {t('notifications.markAllRead')}
            </Button>
          </div>
          <ul
            ref={list}
            aria-label={t('notifications.title')}
            className="flex flex-col divide-y divide-line rounded-card border border-line"
          >
            {items.map((n) => (
              <MemberNotificationItem
                key={n.id}
                n={n}
                request={n.requestId != null ? byId.get(n.requestId) : undefined}
                onOpen={open}
              />
            ))}
          </ul>
        </>
      )}
      {history.hasNextPage && (
        <Button
          variant="secondary"
          className="self-start"
          // Not disabled while loading: a disabled button would drop focus to <body>.
          aria-busy={history.isFetchingNextPage}
          onClick={() => {
            if (history.isFetchingNextPage) return
            setFocusIndex(items.length)
            // On failure focus stays on this button; nothing jumps later.
            void history.fetchNextPage().then((r) => r.isError && setFocusIndex(null))
          }}
        >
          {history.isFetchingNextPage ? t('status.loading') : t('notifications.showOlder')}
        </Button>
      )}
      {history.isFetchNextPageError && (
        <p role="alert" className="text-sm text-text-2">
          {t('notifications.olderFailed')}
        </p>
      )}
    </div>
  )
}
