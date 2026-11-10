import i18n from 'i18next'
import {
  CheckIcon,
  ChevronDownIcon,
  CircleMinusIcon,
  EyeIcon,
  EyeOffIcon,
  FilterXIcon,
  InboxIcon,
  PlayIcon,
  RepeatIcon,
  Undo2Icon,
  XIcon,
} from 'lucide-react'
import {
  type ComponentProps,
  type MouseEvent,
  type ReactNode,
  useEffect,
  useId,
  useRef,
  useState,
} from 'react'
import { useTranslation } from 'react-i18next'
import { flushSync } from 'react-dom'
import { Link, type To, useLocation, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

import {
  useCancelRequest,
  useHideRequest,
  useMyRequests,
  useWithdrawRequest,
  type MyRequest,
} from '@/api/discover'
import { followCutoff, useFollows, useUnfollow, type Follow } from '@/api/follows'
import { unitsLabel } from '@/api/requests'
import { useApiErrorMessage } from '@/api/errors'
import { PageHeader } from '@/components/AppShell'
import { CategoryDot } from '@/components/CategoryDot'
import { EmptyState } from '@/components/EmptyState'
import { ErrorNotice } from '@/components/members'
import { Cover } from '@/components/Poster'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { ProgressBar, RequestStateChip } from '@/components/RequestStatus'
import { StateCell, ThumbSlot } from '@/components/RowCells'
import { JourneyChip } from '@/components/StatusJourney'
import { StateChip } from '@/components/StateChip'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Switch } from '@/components/ui/switch'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { formatDate, formatDay, formatDayMonth, formatPercent } from '@/lib/format'
import { albumTitles, useJourneyTitle } from '@/lib/journey'
import { privateToast } from '@/lib/privateGate'
import { scrollBehavior } from '@/lib/motion'
import {
  REQUEST_CATEGORIES,
  type RequestCategory,
  type RequestGroup,
  requestCategories,
  requestGroup,
  shownRequests,
} from '@/lib/requestFilters'
import { useFocusNextOnRemoval } from '@/lib/returnFocus'
import { requestState, type StateKey, stateLabel, type StateView, unitsState } from '@/lib/states'
import { titleRefParam } from '@/lib/seasons'
import { useUrlState } from '@/lib/urlState'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { cn } from '@/lib/utils'

/** `?request=<id>`: the request a notification points to (a number, never a title). */
const ID = z.coerce.number().int().positive()

function Items({ request }: { request: MyRequest }) {
  const { t } = useTranslation(['member', 'common'])
  const journeyTitle = useJourneyTitle()
  // A movie or a scene is one unit: a bar while it loads, nothing else.
  if (request.kind === 'movie' || request.kind === 'scene') {
    const progress = request.items[0]?.progress
    // A stage means nothing is left to load: the chip says it, no bar at 100%.
    return request.state === 'in_progress' && !request.stage && progress != null ? (
      <ProgressBar value={progress} />
    ) : null
  }
  // All seasons or albums where the request is: the chip above says it, list them only.
  const uniform = request.items.every((i) => i.state === request.state && i.progress == null)
  if (uniform) {
    return (
      <p className={cn('text-sm text-text-2', request.kind !== 'artist' && 'type-data')}>
        {unitsLabel(request.items)}
      </p>
    )
  }
  return (
    <ul
      aria-label={t(request.kind === 'artist' ? 'requests.albumsLabel' : 'requests.itemsLabel', {
        title: request.title,
      })}
      className="flex flex-wrap gap-x-5 gap-y-2"
    >
      {request.items.map((item) => (
        // Wraps at 320 px instead of scrolling the page sideways (WCAG 1.4.10).
        <li
          key={item.album?.id ?? item.season ?? 0}
          className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1"
        >
          {item.album ? (
            <span className="text-sm text-text-2">{item.album.title}</span>
          ) : (
            <span className="type-data text-sm text-text-2">
              {t('title.seasonLabel', { number: item.season ?? 0 })}
            </span>
          )}
          <JourneyChip
            title={journeyTitle(request.title, request.kind, item.season, item.album?.title)}
            units={[item]}
            openIn={item.openIn ?? request.openIn}
          >
            <RequestStateChip
              units={[item]}
              state={item.state}
              stage={item.stage}
              arrivingIn={request.arrivingIn}
            />
          </JourneyChip>
          {item.state === 'in_progress' && !item.stage && item.progress != null && (
            <ProgressBar value={item.progress} />
          )}
        </li>
      ))}
    </ul>
  )
}

/** Confirms cancelling a request: nothing is deleted, downloading seasons stay. */
function CancelDialog({ request, onClose }: { request: MyRequest | null; onClose: () => void }) {
  const { t } = useTranslation(['member', 'common'])
  const cancel = useCancelRequest()
  const errorMessage = useApiErrorMessage()
  return (
    <Dialog
      open={request !== null}
      onOpenChange={(open) => !open && !cancel.isPending && onClose()}
    >
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('requests.cancelTitle', { title: request?.title ?? '' })}</DialogTitle>
          <DialogDescription>{t('requests.cancelDescription')}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" disabled={cancel.isPending} onClick={onClose}>
            {t('requests.cancelKeep')}
          </Button>
          <Button
            variant="primary"
            disabled={cancel.isPending}
            onClick={() =>
              request &&
              cancel.mutate(request.id, {
                // A scene's toasts are private: closing the gate dismisses them.
                onSuccess: () => {
                  if (request.kind === 'scene') privateToast(t('requests.cancelled'))
                  else toast(t('requests.cancelled'))
                  onClose()
                },
                onError: (error) =>
                  request.kind === 'scene'
                    ? privateToast(errorMessage(error), {}, 'error')
                    : toast.error(errorMessage(error)),
              })
            }
          >
            {t('requests.cancelConfirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/**
 * Where a request row leads: the title's detail overlay over this page, or a scene's page. The
 * link has a real `href` (new tab, copy link); a plain click opens the overlay through
 * `useTitleSheet`, so closing it goes back instead of leaving a second entry.
 */
function useDetailsTarget(
  request: Pick<MyRequest, 'kind' | 'titleId'>,
): { to: To; onClick?: (event: MouseEvent<HTMLAnchorElement>) => void } | null {
  const location = useLocation()
  const { open } = useTitleSheet()
  const { kind, titleId } = request
  if (titleId == null) return null
  if (kind === 'scene') return { to: `/private/scenes/${encodeURIComponent(titleId)}` }
  // Opening a detail pushes `?title=` and keeps the filters (F6).
  const params = new URLSearchParams(location.search)
  params.set('title', titleRefParam({ kind, id: titleId }))
  // The highlight was for arriving from a notification, not for every later link.
  params.delete('request')
  return {
    to: { pathname: location.pathname, search: `?${params.toString()}` },
    onClick: (event) => {
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
        return
      }
      event.preventDefault()
      open({ kind, id: titleId })
    },
  }
}

/** "Silent Tide (1999)": the title with its year, read with brackets, shown without (MEM-27). */
function TitleWithYear({ title, year }: { title: string; year?: number | null }) {
  return (
    <>
      {title}
      {year != null && (
        <>
          {' '}
          <span className="type-data ml-1 text-sm font-normal text-text-3">
            <span className="sr-only">(</span>
            {year}
            <span className="sr-only">)</span>
          </span>
        </>
      )}
    </>
  )
}

/**
 * `?request=<id>`: the row a notification points to (MEM-21) is marked, and focused once per
 * page visit: not again after a filter change remounts it, and not while the detail overlay
 * over it is open (the overlay has focus; the row gets it once the overlay closes).
 */
function useRowHighlight() {
  const [id] = useUrlState('request', ID, 0)
  const [params] = useSearchParams()
  const handled = useRef<number | null>(null)
  return {
    id,
    focusNow: (rowId: number) => rowId === id && handled.current !== id && !params.has('title'),
    focused: () => {
      handled.current = id
    },
  }
}

/** "Oct 29" this year, "Oct 29, 2025" before. */
function shortDay(iso: string): string {
  return new Date(iso).getFullYear() === new Date().getFullYear()
    ? formatDayMonth(iso)
    : formatDate(iso)
}

/**
 * When a step of one of `kinds` happened, from the first item that has one: a done step, or
 * one that ended the journey (withdrawn, cancelled).
 */
function doneAt(request: MyRequest, kinds: string[]): string | null {
  for (const item of request.items) {
    const step = item.journey.steps.find(
      (s) => kinds.includes(s.step) && (s.status === 'done' || s.status === 'ended') && s.at,
    )
    if (step?.at) return step.at
  }
  return null
}

/** The request's state as one view (the one state table, spec 0008 F1). */
function viewOf(request: MyRequest): StateView {
  const view =
    unitsState(request.items) ?? requestState(request.state, request.stage, request.arrivingIn)
  // Without journey steps a single unit still has its downloaded share.
  const progress = request.items.length === 1 ? request.items[0]?.progress : null
  return view.key === 'downloading' && view.percent == null && progress != null
    ? { ...view, percent: progress }
    : view
}

/**
 * After the last row of a section went (hidden, withdrawn, cancelled): the first control of
 * the next section, else of the previous one, else the jump links.
 */
function nextSectionControl(row: HTMLElement): HTMLElement | null {
  const section = row.closest('section')
  const first = (el: Element | null) =>
    el?.querySelector<HTMLElement>('a[href], button:not([disabled])') ?? null
  for (let s = section?.nextElementSibling; s; s = s.nextElementSibling) {
    if (s.tagName === 'SECTION' && first(s)) return first(s)
  }
  for (let s = section?.previousElementSibling; s; s = s.previousElementSibling) {
    if (s.tagName === 'SECTION' && first(s)) return first(s)
    if (s.tagName === 'NAV' && first(s)) return first(s)
  }
  return null
}

/** A ready request's state: available, or how much of it is there ("1 of 2 seasons"). */
function readyView(request: MyRequest): StateView {
  if (request.state === 'available') return { key: 'available' }
  const units = request.items.length
  if (units < 2) return { key: 'partlyAvailable' }
  return {
    key: 'partlyAvailable',
    have: request.items.filter((i) => i.state === 'available').length,
    total: units,
    unit: request.kind === 'artist' ? 'albums' : 'seasons',
  }
}

/** States on the way that the journey bar shows, by their step of four. */
const BAR_STEP: Partial<Record<StateKey, number>> = {
  approved: 1,
  queued: 1,
  searching: 1,
  downloading: 2,
  importing: 2,
  arriving: 2,
}
const BAR_STEPS = 4

/**
 * On its way, under the state chip: four segments (requested, approved, getting it, ready), the
 * passed ones filled, the current one tinted (and filled by the downloaded share), then the step
 * and the share: one line like the other sections' detail, wrapping only in a narrow column.
 * The chip above names the state, so the bar's name is the step and the share.
 */
function JourneyBar({ view, step }: { view: StateView; step: number }) {
  const { t } = useTranslation('member')
  const share = view.key === 'downloading' && view.percent != null ? view.percent : null
  const position = t('requests.journeyStep', { step: step + 1, total: BAR_STEPS })
  return (
    <div
      role="img"
      aria-label={share != null ? `${position}, ${formatPercent(share)}` : position}
      className="flex min-h-5 flex-wrap items-center gap-x-2.5 gap-y-1 text-xs text-text-3"
    >
      <span aria-hidden className="grid w-28 min-w-12 shrink grid-cols-4 gap-1">
        {Array.from({ length: BAR_STEPS }, (_, i) => (
          <span
            key={i}
            className={cn(
              'relative h-1 overflow-hidden rounded-full',
              i < step ? 'bg-text-3' : i === step ? 'bg-info/25' : 'bg-line',
            )}
          >
            {i === step && share != null && (
              <span
                className="absolute inset-y-0 left-0 rounded-full bg-info"
                style={{ width: `${Math.round(share * 100)}%` }}
              />
            )}
          </span>
        ))}
      </span>
      <span aria-hidden className="whitespace-nowrap">
        {position}
      </span>
      {share != null && (
        <span aria-hidden className="type-data font-medium text-text-2">
          {formatPercent(share)}
        </span>
      )}
    </div>
  )
}

/**
 * Every row's grid, requests and follows alike (spec 0008, amendment "uniform rows"): one
 * 48 × 72 thumb slot, title and meta, the state (chip over one detail line), and one fixed
 * action column. Phone: the thumb beside title and state, the actions in a row below them.
 */
const ROW =
  'grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-3.5 gap-y-2 border-b border-line py-4 md:grid-cols-[auto_minmax(0,1.15fr)_minmax(0,1fr)_19rem] md:gap-x-5'

/**
 * The thumb slot, 48 × 72 in every row: a 2:3 poster fills it, a square music cover sits
 * centred in it, so titles start on the same x in every section. A pointer shortcut to the
 * row's one link (MEM-27), hidden from assistive technology.
 */
function Thumb({
  onClick,
  clickable,
  dim = false,
  children,
}: {
  onClick: () => void
  clickable: boolean
  dim?: boolean
  children: ReactNode
}) {
  return (
    <ThumbSlot
      size="row"
      aria-hidden
      onClick={onClick}
      className={cn('row-span-2 md:row-span-1', clickable && 'cursor-pointer', dim && 'opacity-55')}
    >
      {children}
    </ThumbSlot>
  )
}

/**
 * The action column: the row's actions right-aligned, then the hide slot, reserved in every row
 * so nothing shifts between rows with and without it. Phone: a row under title and state.
 */
function RowActions({
  children,
  hide,
  empty = false,
}: {
  children?: ReactNode
  hide?: ReactNode
  /** No control at all: the phone layout leaves the row out instead of an empty strip. */
  empty?: boolean
}) {
  return (
    <div
      data-actions
      className={cn(
        'col-start-2 flex min-w-0 items-center gap-1 md:col-start-auto',
        empty && 'max-md:hidden',
      )}
    >
      {/* Two actions (rare: partly there and still cancellable) wrap instead of overflowing. */}
      <div className="-ml-1 flex min-w-0 flex-1 flex-wrap items-center gap-1 md:ml-0 md:justify-end">
        {children}
      </div>
      <div data-hide-slot className="grid size-8.5 shrink-0 place-items-center max-lg:size-11">
        {hide}
      </div>
    </div>
  )
}

/** An icon-only row action (hide, show again): a name for assistive technology and a tooltip. */
function IconAction({
  label,
  tooltip,
  children,
  ...props
}: Omit<ComponentProps<typeof Button>, 'aria-label'> & { label: string; tooltip: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button size="icon" variant="ghost" aria-label={label} className="size-8.5" {...props}>
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{tooltip}</TooltipContent>
    </Tooltip>
  )
}

/** A labelled row action: all of them in the same quiet style, icon and label. */
function RowAction({ className, ...props }: ComponentProps<typeof Button>) {
  return <Button size="sm" variant="secondary" className={cn('min-w-0', className)} {...props} />
}

/** Partly there: one pill per season or album, the ready ones marked. */
function UnitPills({ request }: { request: MyRequest }) {
  const { t } = useTranslation('member')
  return (
    <ul
      aria-label={t(request.kind === 'artist' ? 'requests.albumsLabel' : 'requests.itemsLabel', {
        title: request.title,
      })}
      className="mt-2 flex flex-wrap gap-1"
    >
      {request.items.map((item) => {
        const view = unitsState([item]) ?? requestState(item.state, item.stage)
        const ready = view.key === 'available'
        const share = view.key === 'downloading' ? (view.percent ?? item.progress ?? null) : null
        return (
          <li
            key={item.album?.id ?? item.season ?? 0}
            className={cn(
              'type-data inline-flex h-5.5 max-w-full items-center gap-1 rounded-full px-2 text-xs font-medium',
              ready
                ? 'bg-[color-mix(in_oklab,var(--color-ok)_16%,var(--color-night-1))] text-[color-mix(in_oklab,var(--color-ok)_25%,white)]'
                : 'bg-night-2 text-text-2',
            )}
          >
            {ready && <CheckIcon aria-hidden className="size-3 text-ok" strokeWidth={2.25} />}
            <span aria-hidden={item.album ? undefined : true} className="truncate">
              {item.album
                ? item.album.title
                : t('requests.seasonShort', { number: item.season ?? 0 })}
            </span>
            <span className="sr-only">
              {item.album ? '' : t('title.seasonLabel', { number: item.season ?? 0 })}:{' '}
              {stateLabel(view)}
            </span>
            {!ready && (
              <span aria-hidden>· {share != null ? formatPercent(share) : stateLabel(view)}</span>
            )}
          </li>
        )
      })}
    </ul>
  )
}

/** A sentence for the member under the state: what happens next, in their words. */
function rowNote(request: MyRequest, view: StateView, group: RequestGroup): string | null {
  const t = i18n.t
  if (group === 'waiting')
    return request.canWithdraw ? t('member:requests.notes.canWithdraw') : null
  if (view.key === 'stuckAtImport' || view.key === 'needsAttention') {
    return t('member:requests.notes.stuck')
  }
  if (view.key === 'failed') return t('member:requests.notes.failed')
  if (group === 'ready') {
    const at = doneAt(request, ['ready'])
    return at ? t('member:requests.notes.readySince', { date: shortDay(at) }) : null
  }
  if (view.key === 'withdrawn') {
    const at = doneAt(request, ['withdrawn', 'cancelled'])
    return at
      ? t('member:requests.notes.withdrawnOn', { date: shortDay(at) })
      : t('member:requests.notes.withdrawn')
  }
  if (view.key === 'removed') return t('member:requests.notes.removed')
  return null
}

function RequestRow({
  request,
  group,
  onCancel,
  highlighted = false,
  focusNow = false,
  onFocused,
}: {
  request: MyRequest
  group: RequestGroup
  onCancel: (request: MyRequest) => void
  /** A notification pointed here (`?request=`): marked (MEM-21). */
  highlighted?: boolean
  /** Scroll to the row and focus its link now (once, see `useRowHighlight`). */
  focusNow?: boolean
  onFocused?: () => void
}) {
  const { t } = useTranslation(['member', 'common'])
  const withdraw = useWithdrawRequest()
  const hide = useHideRequest()
  const errorMessage = useApiErrorMessage()
  // Hiding removes the row: focus goes on to the next one (MEM-03).
  const { ref: rowRef, arm, disarm } = useFocusNextOnRemoval<HTMLLIElement>(nextSectionControl)
  const journeyTitle = useJourneyTitle()
  const target = useDetailsTarget(request)
  const link = useRef<HTMLAnchorElement>(null)
  // A scene's toasts are private: closing the gate dismisses them (spec 0008 F2).
  const notify = (message: string) =>
    request.kind === 'scene' ? privateToast(message) : toast(message)
  const notifyError = (message: string) =>
    request.kind === 'scene' ? privateToast(message, {}, 'error') : toast.error(message)

  useEffect(() => {
    if (!focusNow) return
    const el = link.current
    el?.scrollIntoView?.({ block: 'center' })
    el?.focus({ preventScroll: true })
    onFocused?.()
  }, [focusNow, onFocused])

  // Ready to watch says what is there, even while a further season is still on its way.
  const view = group === 'ready' ? readyView(request) : viewOf(request)
  const barStep = group === 'active' ? BAR_STEP[view.key] : undefined
  const note = rowNote(request, view, group)
  const multi = request.kind !== 'movie' && request.kind !== 'scene' && request.items.length > 1
  // All units where the request is: the units line in the meta says it all.
  const uniform = request.items.every((i) => i.state === request.state && i.progress == null)
  // Where it can be watched or listened to: the request's link, else a music album's.
  const openIn = request.openIn[0] ?? request.items.flatMap((i) => i.openIn ?? [])[0]
  // Waiting for approval only: withdraw. Approved and not downloading: cancel.
  const withdrawable =
    request.canWithdraw && !request.items.some((i) => i.state !== 'pending_approval')
  const hasControls =
    (group === 'ready' && openIn !== undefined) ||
    withdrawable ||
    request.canCancel ||
    request.canHide

  // The chip is the state; on its way the bar below carries the downloaded share.
  const chip =
    group === 'ready' ? (
      <StateChip state={view} />
    ) : (
      <JourneyChip
        title={journeyTitle(request.title, request.kind)}
        units={request.items}
        openIn={request.openIn}
        albums={albumTitles(request.items)}
      >
        <StateChip state={view.key === 'downloading' ? { key: 'downloading' } : view} />
      </JourneyChip>
    )

  return (
    <li
      ref={rowRef}
      data-highlighted={highlighted || undefined}
      className={cn(ROW, highlighted && '-mx-3 rounded-card bg-night-1 px-3 ring-1 ring-line')}
    >
      <Thumb
        onClick={() => link.current?.click()}
        clickable={target !== null}
        dim={group === 'closed'}
      >
        <Cover
          file={request.poster}
          title={request.title}
          sizes="48px"
          // Music shows its first album's cover: square, not an artist's circle.
          kind={request.kind === 'artist' ? 'album' : request.kind}
          category={request.category}
        />
      </Thumb>
      <div className="flex min-w-0 flex-col gap-0.5">
        <h3 className="type-display text-lg leading-snug font-bold text-text">
          {target ? (
            <Link
              ref={link}
              to={target.to}
              onClick={target.onClick}
              // The hit area reaches 44 px without moving the row (touch rule).
              className="relative underline-offset-4 after:absolute after:inset-x-0 after:-inset-y-3 hover:underline"
            >
              <TitleWithYear title={request.title} year={request.year} />
            </Link>
          ) : (
            <TitleWithYear title={request.title} year={request.year} />
          )}
        </h3>
        <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-text-3">
          <span className="flex items-center gap-1.5">
            <CategoryDot category={request.category} />
            {t(`categories.${request.category}`)}
          </span>
          {multi && !(group === 'ready') && unitsLabel(request.items) && (
            <span className={cn(request.kind !== 'artist' && 'type-data')}>
              {unitsLabel(request.items)}
            </span>
          )}
          <span>{t('requests.requestedOn', { date: formatDate(request.createdAt) })}</span>
          {request.follow && (
            <span className="flex items-center gap-1.5">
              <RepeatIcon aria-hidden className="size-3.5" strokeWidth={1.75} />
              {request.kind === 'artist'
                ? t('requests.followingArtist')
                : request.kind === 'scene'
                  ? t('requests.foundByFollow')
                  : t('requests.following')}
            </span>
          )}
          {request.hidden && <StatusChip tone="neutral">{t('requests.hiddenChip')}</StatusChip>}
        </p>
      </div>
      <StateCell className="col-start-2 md:col-start-auto">
        {/* Always two lines: the chip, then one detail (the bar, a sentence, a reason). */}
        {chip}
        {barStep !== undefined && <JourneyBar view={view} step={barStep} />}
        {note && <p className="text-sm text-text-2">{note}</p>}
        {request.rejectReason && (
          <p className="max-w-prose border-l-2 border-line pl-2.5 text-sm text-text-2">
            {t('requests.rejectReason', { reason: request.rejectReason })}
          </p>
        )}
        {request.revokeReason && (
          <p className="max-w-prose border-l-2 border-bad pl-2.5 text-sm text-text-2">
            {t('requests.revokeReason', { reason: request.revokeReason })}
          </p>
        )}
        {multi && group === 'ready' ? (
          <UnitPills request={request} />
        ) : (
          multi && group !== 'closed' && !uniform && <Items request={request} />
        )}
      </StateCell>
      {/* The actions in one fixed column, never gold: rows are of equal rank. */}
      <RowActions
        empty={!hasControls}
        hide={
          request.canHide &&
          (request.hidden ? (
            <IconAction
              label={t('requests.unhideLabel', { title: request.title })}
              tooltip={t('requests.unhide')}
              disabled={hide.isPending}
              onClick={() =>
                hide.mutate(
                  { id: request.id, hidden: false },
                  { onError: (error) => notifyError(errorMessage(error)) },
                )
              }
            >
              <EyeIcon aria-hidden />
            </IconAction>
          ) : (
            <IconAction
              label={t('requests.hideLabel', { title: request.title })}
              tooltip={t('requests.hideTooltip')}
              disabled={hide.isPending}
              onClick={() => {
                arm()
                hide.mutate(
                  { id: request.id, hidden: true },
                  {
                    onSuccess: () => notify(t('requests.hidden')),
                    onError: (error) => {
                      disarm()
                      notifyError(errorMessage(error))
                    },
                  },
                )
              }}
            >
              <EyeOffIcon aria-hidden />
            </IconAction>
          ))
        }
      >
        {group === 'ready' && openIn && (
          // A long server name truncates rather than pushing the column wider.
          <RowAction asChild className="max-w-full shrink">
            <a href={openIn.url} target="_blank" rel="noreferrer">
              <PlayIcon aria-hidden className="fill-current" />
              <span className="truncate">{t('title.openIn', { service: openIn.name })}</span>
            </a>
          </RowAction>
        )}
        {withdrawable ? (
          <RowAction
            disabled={withdraw.isPending}
            aria-label={t('requests.withdrawLabel', { title: request.title })}
            onClick={() => {
              // Withdrawn, the row moves to "Closed": focus goes on to the next row.
              arm()
              withdraw.mutate(request.id, {
                onSuccess: () => notify(t('requests.withdrawn')),
                onError: (error) => {
                  disarm()
                  notifyError(errorMessage(error))
                },
              })
            }}
          >
            <Undo2Icon aria-hidden />
            {t('requests.withdraw')}
          </RowAction>
        ) : (
          request.canCancel && (
            <RowAction
              aria-label={t('requests.cancelLabel', { title: request.title })}
              onClick={() => {
                // Cancelled, the row moves to "Closed" after the dialog gave focus back here.
                arm()
                onCancel(request)
              }}
            >
              <XIcon aria-hidden />
              {t('requests.cancel')}
            </RowAction>
          )
        )}
      </RowActions>
    </li>
  )
}

function FollowRow({ follow }: { follow: Follow }) {
  const { t } = useTranslation(['member', 'common'])
  const unfollow = useUnfollow()
  const errorMessage = useApiErrorMessage()
  const cutoff = followCutoff(follow)
  const target = useDetailsTarget(follow)
  const link = useRef<HTMLAnchorElement>(null)

  return (
    // The requests' grid, thumb slot and action column; a follow has no state of its own.
    <li className={ROW}>
      <Thumb onClick={() => link.current?.click()} clickable={target !== null}>
        <Cover
          kind={follow.kind}
          file={follow.poster}
          title={follow.title}
          sizes="48px"
          category={follow.category}
        />
      </Thumb>
      <div className="flex min-w-0 flex-col gap-0.5">
        <h3 className="type-display text-lg leading-snug font-bold text-text">
          {target ? (
            <Link
              ref={link}
              to={target.to}
              onClick={target.onClick}
              className="relative underline-offset-4 after:absolute after:inset-x-0 after:-inset-y-3 hover:underline"
            >
              <TitleWithYear title={follow.title} year={follow.year} />
            </Link>
          ) : (
            <TitleWithYear title={follow.title} year={follow.year} />
          )}
        </h3>
        <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-text-3">
          <span className="flex items-center gap-1.5">
            <CategoryDot category={follow.category} />
            {follow.knownThrough != null
              ? t('follows.afterDay', { date: formatDay(follow.knownThrough) })
              : cutoff === null
                ? t('follows.afterAll')
                : t('follows.after', { season: cutoff })}
          </span>
          <span>{t('follows.since', { date: formatDate(follow.createdAt) })}</span>
        </p>
      </div>
      <div aria-hidden className="max-md:hidden" />
      <RowActions>
        <RowAction
          disabled={unfollow.isPending}
          aria-label={t('follows.unfollowLabel', { title: follow.title })}
          onClick={() =>
            unfollow.mutate(follow.id, {
              onSuccess: () => toast(t('follows.unfollowed', { title: follow.title })),
              onError: (error) => toast.error(errorMessage(error)),
            })
          }
        >
          <CircleMinusIcon aria-hidden />
          {t('follows.unfollow')}
        </RowAction>
      </RowActions>
    </li>
  )
}

/** "Following": the member's follows of series and artists, each with unfollow (user story 4). */
function FollowingSection() {
  const { t } = useTranslation(['member', 'common'])
  const follows = useFollows()
  const errorMessage = useApiErrorMessage()

  return (
    <section aria-labelledby="following-heading" className="mt-12 flex flex-col gap-2">
      <div className="flex flex-col gap-0.5">
        <h2
          id="following-heading"
          className="type-display flex items-center gap-2 text-xl font-bold text-text"
        >
          <RepeatIcon aria-hidden className="size-4 text-text-3" strokeWidth={1.75} />
          {t('follows.title')}
        </h2>
        <p className="text-sm text-text-3">{t('follows.description')}</p>
      </div>
      {follows.isError && <ErrorNotice>{errorMessage(follows.error)}</ErrorNotice>}
      {follows.data?.length === 0 && (
        <p className="py-3 text-sm text-text-2">{t('follows.empty')}</p>
      )}
      {follows.data && follows.data.length > 0 && (
        <ul className="mt-1 border-t border-line">
          {follows.data.map((follow) => (
            <FollowRow key={follow.id} follow={follow} />
          ))}
        </ul>
      )}
    </section>
  )
}

/** A placeholder in the rows' shape while the list loads (F10). */
function RowsSkeleton() {
  const { t } = useTranslation('common')
  return (
    <ul aria-busy aria-label={t('status.loading')} className="mt-9 border-t border-line">
      {[0, 1, 2].map((i) => (
        <li key={i} className="flex items-center gap-3.5 border-b border-line py-4 md:gap-5">
          <Skeleton className="h-18 w-12 shrink-0 rounded-card" />
          <div className="flex flex-1 flex-col gap-2.5">
            <Skeleton className="h-5 w-48 max-w-full" />
            <Skeleton className="h-4 w-64 max-w-full" />
          </div>
        </li>
      ))}
    </ul>
  )
}

/** One row of filter chips: toggle buttons in a labelled group, each with its count. */
function ChipGroup<V extends string>({
  label,
  options,
  value,
  onChange,
}: {
  label: string
  options: { value: V; label: string; count: number; dot?: RequestCategory }[]
  value: V
  onChange: (next: V) => void
}) {
  return (
    <div role="group" aria-label={label} className="-mx-1 flex flex-wrap gap-1 px-1 py-0.5">
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          aria-pressed={value === option.value}
          onClick={() => onChange(option.value)}
          className={cn(
            'flex h-8 shrink-0 cursor-pointer items-center gap-2 rounded-full px-3.5 text-sm font-medium whitespace-nowrap text-text-2 transition-colors hover:text-text max-lg:h-11',
            value === option.value ? 'bg-night-2 text-text' : 'hover:bg-night-1',
          )}
        >
          {option.dot && <CategoryDot category={option.dot} />}
          {option.label}
          <span className="type-data text-xs text-text-3">{option.count}</span>
        </button>
      ))}
    </div>
  )
}

/**
 * Movies, series and music the member asked for. Private requests are not here: they live in the
 * private area under `/private/requests` (spec 0008 Q11, PRIV-16).
 */
export default function RequestsPage() {
  const { t } = useTranslation(['member', 'common'])
  const [showHidden, setShowHidden] = useState(false)
  const [cancelling, setCancelling] = useState<MyRequest | null>(null)
  const requests = useMyRequests(showHidden)
  const errorMessage = useApiErrorMessage()
  const toggleId = useId()

  return (
    <div className="max-w-6xl">
      <PageHeader
        variant="member"
        title={t('requests.title')}
        description={t('requests.description')}
      />
      <AllRequests
        requests={requests}
        errorMessage={errorMessage}
        onCancel={setCancelling}
        tools={
          <label
            htmlFor={toggleId}
            className="flex items-center gap-2 text-sm text-text-2 max-lg:min-h-11"
          >
            <Switch id={toggleId} checked={showHidden} onCheckedChange={setShowHidden} />
            {t('requests.showHidden')}
          </label>
        }
      />
      <CancelDialog request={cancelling} onClose={() => setCancelling(null)} />
      {/* Below the list: added once the list is there, so it never pushes it down (CLS). */}
      {!requests.isPending && <FollowingSection />}
    </div>
  )
}

/** The page's sections, in the order of prototype 09-A; "Closed" starts collapsed. */
const PHASES: RequestGroup[] = ['active', 'waiting', 'ready', 'closed']

const CATEGORY = z.enum(['all', ...REQUEST_CATEGORIES])

/** One phase: its heading with the count, a line on what it means, its rows. */
function PhaseSection({
  group,
  count,
  open,
  onToggle,
  children,
}: {
  group: RequestGroup
  count: number
  /** Closed only: whether its rows show. */
  open: boolean
  onToggle?: () => void
  children: ReactNode
}) {
  const { t } = useTranslation('member')
  const id = `requests-${group}`
  const label = (
    <>
      {t(`requests.groups.${group}.title`)}
      <span className="sr-only">, </span>
      <span className="type-data rounded-full bg-night-2 px-2 py-0.5 text-xs font-medium text-text-2">
        {count}
      </span>
    </>
  )
  return (
    <section id={id} aria-labelledby={`${id}-heading`} className="mt-9 scroll-mt-6">
      <h2
        id={`${id}-heading`}
        tabIndex={onToggle ? undefined : -1}
        className="type-display flex items-center gap-2.5 rounded-control text-xl font-extrabold text-text ring-polaris ring-offset-4 ring-offset-night-0 focus:outline-none focus-visible:ring-2"
      >
        {onToggle ? (
          <button
            type="button"
            data-jump-target
            aria-expanded={open}
            aria-controls={open ? `${id}-rows` : undefined}
            onClick={onToggle}
            className="flex cursor-pointer items-center gap-2.5 rounded-control max-lg:min-h-11"
          >
            {label}
            <ChevronDownIcon
              aria-hidden
              className={cn(
                'size-4.5 text-text-3 motion-safe:transition-transform motion-safe:duration-(--dur-2)',
                open && 'rotate-180',
              )}
            />
          </button>
        ) : (
          label
        )}
      </h2>
      <p className="mt-1 text-sm text-text-3">{t(`requests.groups.${group}.lead`)}</p>
      {open && (
        <ul id={`${id}-rows`} className="mt-3 border-t border-line">
          {children}
        </ul>
      )}
    </section>
  )
}

/**
 * Movies, series and music (Q11), grouped by phase (spec 0008 slice 6, amendment 2026-11-07;
 * prototype 09-A): on its way, waiting for approval, ready to watch, then the closed ones
 * collapsed; jump links with counts on top. The category lives in the URL (`?category=`, F6);
 * `?request=` marks the row a notification points to (MEM-21) and opens "Closed" for it.
 */
function AllRequests({
  requests,
  errorMessage,
  onCancel,
  tools,
}: {
  requests: ReturnType<typeof useMyRequests>
  errorMessage: (error: unknown) => string
  onCancel: (request: MyRequest) => void
  /** The page's own controls (Show hidden), beside the category filter. */
  tools: ReactNode
}) {
  const { t } = useTranslation(['member', 'common'])
  const [category, setCategory] = useUrlState('category', CATEGORY, 'all')
  const highlight = useRowHighlight()
  const all = requests.data ?? []
  const shown = shownRequests(all, { state: 'all', category })
  const categories = requestCategories(all)
  const byGroup = (group: RequestGroup) => shown.filter((r) => requestGroup(r.state) === group)
  const highlightedClosed = shown.some(
    (r) => r.id === highlight.id && requestGroup(r.state) === 'closed',
  )
  // Untouched, "Closed" opens for a closed request a notification points to; then the toggle
  // decides.
  const [closedChoice, setClosedOpen] = useState<boolean | null>(null)
  const openClosed = closedChoice ?? highlightedClosed
  // A jump link: open "Closed" first if needed, then scroll its section into view and move
  // focus there.
  const jumpTo = (group: RequestGroup) => {
    if (group === 'closed') flushSync(() => setClosedOpen(true))
    const section = document.getElementById(`requests-${group}`)
    section?.scrollIntoView?.({ block: 'start', behavior: scrollBehavior() })
    section
      ?.querySelector<HTMLElement>('[data-jump-target], h2[tabindex]')
      ?.focus({ preventScroll: true })
  }

  return (
    <>
      {all.length > 0 && (
        <div className="mb-1 flex flex-wrap items-center justify-between gap-3">
          {categories.length > 1 ? (
            <ChipGroup<RequestCategory | 'all'>
              label={t('requests.filters.category')}
              value={category}
              onChange={setCategory}
              options={[
                {
                  value: 'all' as const,
                  label: t('requests.filters.allCategories'),
                  count: all.length,
                },
                ...categories.map((value) => ({
                  value,
                  label: t(`categories.${value}`),
                  count: all.filter((r) => r.category === value).length,
                  dot: value,
                })),
              ]}
            />
          ) : (
            <span />
          )}
          {tools}
        </div>
      )}
      {all.length === 0 && <div className="mb-1 flex justify-end">{tools}</div>}
      {requests.isError && (
        <QueryError
          message={errorMessage(requests.error)}
          onRetry={() => void requests.refetch()}
        />
      )}
      {requests.isPending && <RowsSkeleton />}
      {requests.data?.length === 0 && (
        <EmptyState
          icon={InboxIcon}
          title={t('requests.emptyTitle')}
          description={t('requests.emptyDescription')}
          action={
            <Button asChild>
              <Link to="/">{t('requests.toDiscover')}</Link>
            </Button>
          }
        />
      )}
      {all.length > 0 && shown.length === 0 && (
        <EmptyState
          icon={FilterXIcon}
          title={t('requests.filters.noneTitle')}
          description={t('requests.filters.noneDescription')}
          action={
            <Button onClick={() => setCategory('all')}>{t('requests.filters.showAll')}</Button>
          }
        />
      )}
      {shown.length > 0 && (
        <>
          <nav
            aria-label={t('requests.jumpLabel')}
            className="-mx-4 mt-4 flex gap-2 overflow-x-auto px-4 py-1 [scrollbar-width:none] sm:mx-0 sm:flex-wrap sm:px-0"
          >
            {PHASES.filter((g) => byGroup(g).length > 0).map((group) => (
              <a
                key={group}
                href={`#requests-${group}`}
                onClick={(event) => {
                  event.preventDefault()
                  jumpTo(group)
                }}
                className="inline-flex h-8.5 shrink-0 items-center gap-1.5 rounded-full border border-line bg-night-1 px-3.5 text-sm whitespace-nowrap text-text-2 transition-colors duration-(--dur-1) hover:border-text-3 hover:text-text max-lg:h-11"
              >
                <span className="type-data font-semibold text-text">{byGroup(group).length}</span>{' '}
                {t(`requests.groups.${group}.jump`)}
              </a>
            ))}
          </nav>
          {PHASES.map((group) => {
            const rows = byGroup(group)
            if (rows.length === 0) return null
            const closed = group === 'closed'
            return (
              <PhaseSection
                key={group}
                group={group}
                count={rows.length}
                open={closed ? openClosed : true}
                onToggle={closed ? () => setClosedOpen(!openClosed) : undefined}
              >
                {rows.map((request) => (
                  <RequestRow
                    key={request.id}
                    request={request}
                    group={group}
                    onCancel={onCancel}
                    highlighted={request.id === highlight.id}
                    focusNow={highlight.focusNow(request.id)}
                    onFocused={highlight.focused}
                  />
                ))}
              </PhaseSection>
            )
          })}
        </>
      )}
    </>
  )
}
