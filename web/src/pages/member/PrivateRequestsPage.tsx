import { ExternalLinkIcon, RepeatIcon } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { z } from 'zod'

import {
  type MyRequest,
  useCancelRequest,
  useHideRequest,
  useWithdrawRequest,
} from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { usePrivateRequests } from '@/api/private'
import { CategoryDot } from '@/components/CategoryDot'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import { Cover } from '@/components/Poster'
import { PrivateFollowsSection } from '@/components/PrivateFollows'
import { QueryError } from '@/components/QueryError'
import { ProgressBar, RequestStateChip } from '@/components/RequestStatus'
import { Skeleton } from '@/components/Skeleton'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { waitingForApproval } from '@/lib/detail'
import { formatDate, formatNumber } from '@/lib/format'
import { privateToast } from '@/lib/privateGate'
import { scenePath } from '@/lib/privateNav'
import {
  REQUEST_GROUPS,
  type RequestGroup,
  shownRequests,
  requestGroup,
} from '@/lib/requestFilters'
import { useFocusNextOnRemoval } from '@/lib/returnFocus'
import { useUrlState } from '@/lib/urlState'
import { cn } from '@/lib/utils'

/*
 * `/private/requests` and `/private/following` (spec 0008 W5, PRIV-09, PRIV-16): the private
 * category's requests and follows on their own pages inside the area; they never appear under
 * the general Requests. Copy is about scenes (PRIV-10), toasts are neutral and private (F2).
 */

// The same state groups as the general Requests page (slice 6): actionable first.
const FILTERS = ['all', ...REQUEST_GROUPS] as const
type Filter = 'all' | RequestGroup

function filterOf(request: MyRequest): RequestGroup {
  return requestGroup(request.state)
}

const ID = z.coerce.number().int().positive()

/** The private area's heading row (dot and title). */
function Heading({ title, description }: { title: string; description?: string }) {
  return (
    <div className="mb-8 flex flex-col gap-1.5">
      <h1
        tabIndex={-1}
        className="type-display flex items-center gap-3 text-3xl font-extrabold text-text outline-none sm:text-4xl"
      >
        <CategoryDot category="adult" className="size-2.5" />
        {title}
      </h1>
      {description && <p className="max-w-prose text-base text-text-2">{description}</p>}
    </div>
  )
}

/** Cancelling a scene request: scene copy, an alert dialog (PRIV-10). */
function CancelDialog({ request, onClose }: { request: MyRequest | null; onClose: () => void }) {
  const { t } = useTranslation('member')
  const cancel = useCancelRequest()
  const errorMessage = useApiErrorMessage()
  return (
    <ConfirmDialog
      open={request !== null}
      onOpenChange={(open) => !open && !cancel.isPending && onClose()}
      title={t('privateRequests.cancelTitle')}
      description={t('privateRequests.cancelDescription')}
      confirmLabel={t('privateRequests.cancelConfirm')}
      pending={cancel.isPending}
      onConfirm={() =>
        request &&
        cancel.mutate(request.id, {
          onSuccess: () => {
            privateToast(t('privateRequests.cancelled'))
            onClose()
          },
          onError: (error) => privateToast(errorMessage(error), {}, 'error'),
        })
      }
    />
  )
}

function RequestRow({
  request,
  onCancel,
  highlighted,
}: {
  request: MyRequest
  onCancel: (request: MyRequest) => void
  /** A notification pointed here (`?request=`): marked, scrolled to and focused once. */
  highlighted: boolean
}) {
  const { t } = useTranslation('member')
  const withdraw = useWithdrawRequest()
  const hide = useHideRequest()
  const errorMessage = useApiErrorMessage()
  const { ref, arm, disarm } = useFocusNextOnRemoval<HTMLLIElement>()
  const path = request.titleId ? scenePath(request.titleId) : null
  const progress = request.items[0]?.progress
  const notifyError = (error: unknown) => privateToast(errorMessage(error), {}, 'error')
  const waitingOnly = waitingForApproval(request)
  const title = useRef<HTMLAnchorElement>(null)
  useEffect(() => {
    if (!highlighted) return
    title.current?.scrollIntoView?.({ block: 'center' })
    title.current?.focus({ preventScroll: true })
  }, [highlighted])
  const thumb = <Cover file={request.poster} title={request.title} sizes="168px" category="adult" />
  return (
    <li
      ref={ref}
      data-highlighted={highlighted || undefined}
      className="grid data-[highlighted]:rounded-card data-[highlighted]:bg-night-1 data-[highlighted]:px-3 grid-cols-[7.5rem_minmax(0,1fr)] items-start gap-4 py-5 sm:grid-cols-[10.5rem_minmax(0,1fr)_auto] sm:items-center sm:gap-5"
    >
      {path ? (
        <Link
          to={path}
          tabIndex={-1}
          aria-hidden
          className="block rounded-card ring-polaris ring-offset-2 ring-offset-night-0 hover:ring-2"
        >
          {thumb}
        </Link>
      ) : (
        thumb
      )}
      <div className="flex min-w-0 flex-col gap-1.5">
        <h2 className="type-display text-lg leading-snug font-bold text-text">
          {path ? (
            <Link
              ref={title}
              to={path}
              className="relative rounded-control underline-offset-4 after:absolute after:inset-x-0 after:-inset-y-3 hover:underline"
            >
              {request.title}
            </Link>
          ) : (
            request.title
          )}
        </h2>
        <p className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-sm text-text-3">
          <RequestStateChip units={request.items} state={request.state} stage={request.stage} />
          <span className="type-data">
            {t('requests.requestedOn', { date: formatDate(request.createdAt) })}
          </span>
          {request.follow && (
            <span className="flex items-center gap-1.5">
              <RepeatIcon aria-hidden className="size-3.5" strokeWidth={1.75} />
              {t('requests.foundByFollow')}
            </span>
          )}
          {request.hidden && <StatusChip tone="neutral">{t('requests.hiddenChip')}</StatusChip>}
        </p>
        {request.state === 'in_progress' && !request.stage && progress != null && (
          <ProgressBar value={progress} />
        )}
        {request.rejectReason && (
          <p className="max-w-prose border-l-2 border-line pl-3 text-sm text-text-2">
            {t('requests.rejectReason', { reason: request.rejectReason })}
          </p>
        )}
        {request.revokeReason && (
          <p className="max-w-prose border-l-2 border-bad pl-3 text-sm text-text-2">
            {t('requests.revokeReason', { reason: request.revokeReason })}
          </p>
        )}
      </div>
      <div className="col-span-2 -ml-3 flex flex-wrap gap-1 empty:hidden sm:col-span-1 sm:ml-0 sm:justify-end">
        {request.state === 'available' &&
          request.openIn.map((link) => (
            <Button key={link.url} asChild size="sm" variant="ghost">
              <a href={link.url} target="_blank" rel="noreferrer">
                <ExternalLinkIcon aria-hidden />
                {t('private.openIn', { name: link.name })}
              </a>
            </Button>
          ))}
        {waitingOnly ? (
          <Button
            size="sm"
            variant="ghost"
            disabled={withdraw.isPending}
            aria-label={t('requests.withdrawLabel', { title: request.title })}
            onClick={() =>
              withdraw.mutate(request.id, {
                onSuccess: () => privateToast(t('privateRequests.withdrawn')),
                onError: notifyError,
              })
            }
          >
            {t('requests.withdraw')}
          </Button>
        ) : (
          request.canCancel && (
            <Button
              size="sm"
              variant="ghost"
              aria-label={t('requests.cancelLabel', { title: request.title })}
              onClick={() => onCancel(request)}
            >
              {t('requests.cancel')}
            </Button>
          )
        )}
        {request.canHide && (
          <Button
            size="sm"
            variant="ghost"
            disabled={hide.isPending}
            aria-label={t(request.hidden ? 'requests.unhideLabel' : 'requests.hideLabel', {
              title: request.title,
            })}
            onClick={() => {
              arm()
              hide.mutate(
                { id: request.id, hidden: !request.hidden },
                {
                  onSuccess: () => !request.hidden && privateToast(t('privateRequests.hidden')),
                  onError: (error) => {
                    disarm()
                    notifyError(error)
                  },
                },
              )
            }}
          >
            {t(request.hidden ? 'requests.unhide' : 'requests.hide')}
          </Button>
        )}
      </div>
    </li>
  )
}

/** `/private/requests`: the member's scene requests, filtered by where they are (in the URL). */
export function PrivateRequestsPage() {
  const { t } = useTranslation(['member', 'common'])
  const [showHidden, setShowHidden] = useState(false)
  const [cancelling, setCancelling] = useState<MyRequest | null>(null)
  const [filter, setFilter] = useUrlState<Filter>('state', z.enum(FILTERS), 'all')
  const [highlight] = useUrlState('request', ID, 0)
  const requests = usePrivateRequests(showHidden)
  const errorMessage = useApiErrorMessage()
  const toggleId = useId()
  const all = requests.data ?? []
  const shown = shownRequests(all, { state: filter, category: 'all' })
  return (
    <>
      <Heading title={t('privateRequests.title')} description={t('privateRequests.description')} />
      <div className="mb-2 flex max-w-4xl flex-wrap items-center justify-between gap-3">
        <div
          role="group"
          aria-label={t('privateRequests.filterLabel')}
          className="-mx-1 flex gap-1 overflow-x-auto px-1 py-1"
        >
          {FILTERS.map((value) => {
            const count =
              value === 'all' ? all.length : all.filter((r) => filterOf(r) === value).length
            return (
              <button
                key={value}
                type="button"
                aria-pressed={filter === value}
                onClick={() => setFilter(value)}
                className={cn(
                  'flex h-9 shrink-0 cursor-pointer items-center gap-2 rounded-full px-3.5 text-sm max-lg:h-11 font-medium whitespace-nowrap text-text-2 transition-colors duration-(--dur-1) hover:text-text',
                  filter === value ? 'bg-night-2 text-text' : 'hover:bg-night-1',
                )}
              >
                {t(`requests.filters.states.${value}`)}
                {requests.data && (
                  <span className="type-data text-xs text-text-3">{formatNumber(count)}</span>
                )}
              </button>
            )
          })}
        </div>
        <label htmlFor={toggleId} className="flex items-center gap-2 text-sm text-text-2">
          <Switch id={toggleId} checked={showHidden} onCheckedChange={setShowHidden} />
          {t('requests.showHidden')}
        </label>
      </div>
      {requests.isError && (
        <QueryError
          message={errorMessage(requests.error)}
          onRetry={() => void requests.refetch()}
        />
      )}
      {requests.isPending && (
        <ul aria-busy className="flex max-w-4xl flex-col gap-5 py-5">
          {Array.from({ length: 3 }, (_, i) => (
            <li key={i} aria-hidden className="flex gap-5">
              <Skeleton className="aspect-video w-30 rounded-card sm:w-42" />
              <span className="flex flex-1 flex-col gap-2">
                <Skeleton className="h-5 w-1/2 rounded-full" />
                <Skeleton className="h-4 w-1/3 rounded-full" />
              </span>
            </li>
          ))}
        </ul>
      )}
      {requests.data && shown.length === 0 && (
        <div className="flex max-w-4xl flex-col items-start gap-4 rounded-card border border-dashed border-line px-5 py-8">
          <p className="text-base text-text-2">
            {all.length === 0 ? t('privateRequests.empty') : t('privateRequests.emptyFilter')}
          </p>
          <Button asChild variant="ghost" size="sm">
            <Link to="/private">{t('private.backToBrowsing')}</Link>
          </Button>
        </div>
      )}
      {shown.length > 0 && (
        <ul className="flex max-w-4xl flex-col divide-y divide-line">
          {shown.map((request) => (
            <RequestRow
              key={request.id}
              request={request}
              onCancel={setCancelling}
              highlighted={request.id === highlight}
            />
          ))}
        </ul>
      )}
      <CancelDialog request={cancelling} onClose={() => setCancelling(null)} />
    </>
  )
}

/** `/private/following`: performers and studios the member follows, with the weekly cap. */
export function PrivateFollowingPage() {
  const { t } = useTranslation('member')
  return (
    <>
      <Heading title={t('privateFollows.title')} description={t('privateFollows.description')} />
      <PrivateFollowsSection />
    </>
  )
}
