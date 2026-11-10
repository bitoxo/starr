import {
  BanIcon,
  CheckIcon,
  CircleAlertIcon,
  EyeOffIcon,
  PauseIcon,
  RotateCwIcon,
  XIcon,
} from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

import type { MediaKind } from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import {
  ACQUISITION_STATES,
  REQUEST_STATES,
  refOf,
  seasonLabel,
  unitsLabel,
  useAcquisitions,
  useAdminRequest,
  useAdminRequests,
  useApproveRequests,
  useRetryAcquisition,
  type Acquisition,
  type AcquisitionState,
  type AdminRequest,
  type CategoryName,
  type RequestState,
} from '@/api/requests'
import { useSession } from '@/auth/session'
import { PageHeader } from '@/components/AppShell'
import { CategoryDot } from '@/components/CategoryDot'
import { type Column, DataTable } from '@/components/DataTable'
import { PosterThumb } from '@/components/PosterThumb'
import { AcquisitionStateChip, ProgressBar, RequestStateChip } from '@/components/RequestStatus'
import { StateCell, ThumbSlot } from '@/components/RowCells'
import { StateChip } from '@/components/StateChip'
import { JourneyCard } from '@/components/StatusJourney'
import { Button } from '@/components/ui/button'
import { Sheet, SheetContent, SheetDescription, SheetTitle } from '@/components/ui/sheet'
import { inboxLink } from '@/lib/attention'
import { formatDateTime, formatRelative } from '@/lib/format'
import { albumTitles, useJourneyTitle } from '@/lib/journey'
import { acquisitionState, requestState, stateLabel, unitsState } from '@/lib/states'
import { useUrlSort, useUrlState } from '@/lib/urlState'
import { cn } from '@/lib/utils'

import { requestLabel, useApprovalFeedback } from './approvals'
import { useCategoryLabel } from './inbox/items'
import { ReasonDialog } from './ReasonDialog'

/*
 * Requests and Downloads (spec 0008 F5, Q22): both on the shared DataTable with search, filters,
 * sort and the count in the URL (F6), one link per row, the member linked to their page, stacked
 * cards below 768 px, and bulk actions that depend on what is chosen (ADM-10, ADM-31). The
 * request detail has an action bar per state (ADM-04). Search text is kept in the URL only while
 * the private category is locked, so no private term lands in a URL outside `/private` (F6).
 */

const CATEGORIES = ['movies', 'series', 'music', 'adult'] as const satisfies CategoryName[]

/**
 * The table's search text: in the URL while the private category is locked (a locked admin
 * cannot search private titles anyway), only in the page while it is open.
 */
function useSearchText(): [string, (value: string) => void] {
  const unlocked = useSession().me?.privateCategory?.unlocked === true
  const [inUrl, setInUrl] = useUrlState('q', z.string(), '')
  const [local, setLocal] = useState(inUrl)
  if (unlocked) {
    return [
      local,
      (value) => {
        setLocal(value)
        if (inUrl) setInUrl('')
      },
    ]
  }
  return [
    inUrl,
    (value) => {
      setLocal(value)
      setInUrl(value)
    },
  ]
}

function Segmented<T extends string>({
  label,
  options,
  value,
  onChange,
  render,
}: {
  label: string
  options: readonly T[]
  value: T
  onChange: (value: T) => void
  render: (value: T) => string
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className="inline-flex flex-wrap rounded-full border border-line bg-night-1 p-0.5"
    >
      {options.map((option) => (
        <button
          key={option}
          type="button"
          role="radio"
          aria-checked={value === option}
          onClick={() => onChange(option)}
          className={cn(
            'min-h-8 cursor-pointer rounded-full px-3 py-1 text-sm text-text-2 transition-colors hover:text-text max-lg:min-h-11',
            value === option && 'bg-night-2 text-text',
          )}
        >
          {render(option)}
        </button>
      ))}
    </div>
  )
}

function FilterSelect({
  label,
  value,
  onChange,
  children,
}: {
  label: string
  value: string
  onChange: (value: string) => void
  children: ReactNode
}) {
  return (
    <label className="flex items-center gap-2 text-sm text-text-2">
      <span>{label}</span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="h-9 max-w-48 cursor-pointer rounded-full max-lg:h-11 border border-line bg-night-1 px-3 text-sm text-text hover:border-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-polaris/40"
      >
        {children}
      </select>
    </label>
  )
}

/** What an acquisition's title is: the movie, the season's series or the album's artist. */
function acquisitionKind(acquisition: Acquisition): MediaKind {
  if (acquisition.album) return 'artist'
  return acquisition.season != null ? 'series' : 'movie'
}

function TitleCell({
  poster,
  title,
  year,
  category,
  extra,
  onOpen,
  openLabel,
}: {
  poster?: string | null
  title: string
  year?: number | null
  category: CategoryName
  extra?: string | null
  onOpen: () => void
  openLabel: string
}) {
  const label = useCategoryLabel()
  return (
    <div className="flex items-center gap-3">
      {/* One 32 × 48 slot in every row: titles start on the same x, rows keep one height. */}
      <ThumbSlot size="table" aria-hidden>
        <PosterThumb file={poster} title={title} category={category} fill />
      </ThumbSlot>
      <div className="flex min-w-0 flex-col leading-tight">
        <button
          type="button"
          onClick={onOpen}
          aria-label={openLabel}
          className="cursor-pointer truncate text-left font-semibold text-text hover:underline"
        >
          {title}
          {year != null && (
            <span className="type-data ml-2 text-sm font-normal text-text-3">{year}</span>
          )}
        </button>
        <span className="flex items-center gap-2 text-sm text-text-2">
          <CategoryDot category={category} />
          {label(category)}
          {extra && (
            <span className={cn('text-text-3', category !== 'music' && 'type-data')}>{extra}</span>
          )}
        </span>
      </div>
    </div>
  )
}

/** The least progress among a request's downloading items (what it waits for). */
function requestProgress(request: AdminRequest): number | null {
  const values = request.items
    .filter((i) => i.state === 'in_progress' && i.progress != null)
    .map((i) => i.progress as number)
  return values.length ? Math.min(...values) : null
}

const pending = (r: AdminRequest) => r.items.some((i) => i.decision === 'pending_approval')

/**
 * A detail line under the chip: one line, the full text as a tooltip (and in the sheet). Its
 * width is capped at the column's, so a long reason truncates instead of widening the column;
 * the chip above keeps its full width.
 */
function DetailLine({ children }: { children: string }) {
  return (
    <p title={children} className="max-w-full truncate text-xs text-text-2 md:max-w-48">
      {children}
    </p>
  )
}

/**
 * A request's state like the member rows': the chip from the state table on top (a plain one,
 * the journey is in the detail: one tab stop per row, ADM-10), then one detail line where there
 * is one: the download's progress, what a held request waits for, the decline or revoke reason,
 * or the source's message for a failed or stuck one.
 */
function RequestStateCell({ request }: { request: AdminRequest }) {
  const { t } = useTranslation('admin')
  const view =
    unitsState(request.items, { admin: true }) ??
    requestState(request.state, request.stage, request.arrivingIn)
  const progress = view.key === 'downloading' && !request.stage ? requestProgress(request) : null
  // The same items the sheet explains: a failed one's error, a stuck download's message.
  const failed = request.items.filter((i) => i.state === 'failed')
  const lastError = failed.find((i) => i.lastError)?.lastError
  const sourceMessage = request.items.find(
    (i) => i.state === 'in_progress' && i.sourceMessage,
  )?.sourceMessage
  // The sheet offers "Submit again" only for an item that reached the source.
  const retryable = failed.some((i) => i.acquisitionId != null)
  let detail: ReactNode = null
  switch (view.key) {
    case 'downloading':
      if (progress != null) detail = <ProgressBar value={progress} />
      break
    case 'held':
      detail = <DetailLine>{t('requests.row.held')}</DetailLine>
      break
    case 'declined':
      if (request.rejectReason) detail = <DetailLine>{request.rejectReason}</DetailLine>
      break
    case 'revoked':
      if (request.revokeReason) detail = <DetailLine>{request.revokeReason}</DetailLine>
      break
    case 'failed':
      detail = (
        <DetailLine>
          {lastError ?? (retryable ? t('requests.row.failed') : t('requests.detail.failed'))}
        </DetailLine>
      )
      break
    case 'stuckAtImport':
    case 'needsAttention':
      detail = <DetailLine>{sourceMessage ?? t('requests.row.stuck')}</DetailLine>
      break
  }
  return (
    <StateCell className="max-md:items-end">
      {/* The bar below carries the share, so the chip names the state alone. */}
      <StateChip state={progress != null ? { key: 'downloading' } : view} />
      {detail}
    </StateCell>
  )
}

export default function RequestsPage() {
  const { t } = useTranslation(['admin', 'common'])
  // Requests and Downloads are two routes of the same page (spec 0008 Q22).
  const downloads = useLocation().pathname === '/admin/downloads'
  return (
    <>
      <PageHeader
        variant="admin"
        title={downloads ? t('requests.downloadsTitle') : t('requests.title')}
        description={downloads ? t('requests.downloadsDescription') : t('requests.description')}
      />
      {downloads ? <AcquisitionsView /> : <RequestsView />}
    </>
  )
}

function RequestsView() {
  const { t } = useTranslation(['admin', 'common'])
  const errorMessage = useApiErrorMessage()
  const categoryLabel = useCategoryLabel()
  const feedback = useApprovalFeedback()
  const approve = useApproveRequests()
  const [state, setState] = useUrlState<string>('state', z.enum(REQUEST_STATES), '')
  const [category, setCategory] = useUrlState<string>('category', z.enum(CATEGORIES), '')
  const [member, setMember] = useUrlState('member', z.coerce.number().int().positive(), 0)
  const [sort, setSort] = useUrlSort()
  const [open, setOpen] = useUrlState('open', z.coerce.number().int().positive(), 0)
  const [query, setQuery] = useSearchText()
  const requests = useAdminRequests({
    state: (state || undefined) as RequestState | undefined,
    category: (category || undefined) as CategoryName | undefined,
    memberId: member || undefined,
  })
  const list = requests.data
  const [selected, setSelected] = useState<Set<string | number>>(new Set())
  const [revoking, setRevoking] = useState<AdminRequest[] | null>(null)
  const [declining, setDeclining] = useState<AdminRequest[] | null>(null)

  // Members to filter by: those listed (a neutral private row names nobody).
  const members = new Map<number, string>()
  for (const r of list ?? []) if (!r.redacted) members.set(r.member.id, r.member.displayName)

  const columns: Column<AdminRequest>[] = [
    {
      id: 'title',
      header: t('requests.columns.title'),
      sortValue: (r) => r.title,
      className: 'min-w-64',
      cell: (r) => (
        <div>
          <TitleCell
            poster={r.poster}
            title={r.title}
            year={r.year}
            category={r.category}
            extra={unitsLabel(r.items)}
            onOpen={() => setOpen(r.id, { push: true })}
            openLabel={t('requests.openDetails', { title: r.title })}
          />
          {r.hidden && (
            <span className="mt-1 flex items-center gap-1 text-xs text-text-3">
              <EyeOffIcon aria-hidden className="size-3" strokeWidth={1.75} />
              {t('requests.hidden')}
            </span>
          )}
        </div>
      ),
    },
    {
      id: 'member',
      header: t('requests.columns.member'),
      sortValue: (r) => r.member.displayName,
      className: 'w-48',
      cell: (r) =>
        r.redacted ? (
          <span className="text-text-2">{r.member.displayName}</span>
        ) : (
          <Link
            to={`/admin/people/members/${r.member.id}`}
            className="flex flex-col leading-tight hover:underline"
            aria-label={t('requests.openMember', { name: r.member.displayName })}
          >
            <span className="text-text">{r.member.displayName}</span>
            <span className="type-data text-sm text-text-3">{r.member.username}</span>
          </Link>
        ),
    },
    {
      id: 'state',
      header: t('requests.columns.state'),
      className: 'w-56',
      cell: (r) => <RequestStateCell request={r} />,
    },
    {
      id: 'requested',
      header: t('requests.columns.requested'),
      sortValue: (r) => r.createdAt,
      align: 'end',
      className: 'w-36',
      cell: (r) => (
        <time
          dateTime={r.createdAt}
          title={formatDateTime(r.createdAt)}
          className="type-data text-sm text-text-2"
        >
          {formatRelative(r.createdAt)}
        </time>
      ),
    },
  ]

  return (
    <>
      <DataTable<AdminRequest>
        label={t('requests.title')}
        columns={columns}
        rows={list}
        rowKey={(r) => r.id}
        loading={requests.isPending}
        error={requests.isError ? errorMessage(requests.error) : null}
        onRetry={() => void requests.refetch()}
        sort={sort}
        onSortChange={setSort}
        search={{
          label: t('requests.filters.search'),
          value: query,
          onChange: setQuery,
          matches: (r, q) => {
            const needle = q.toLowerCase()
            return [r.title, r.member.displayName, r.member.username].some((v) =>
              v.toLowerCase().includes(needle),
            )
          },
        }}
        filters={
          <>
            <FilterSelect label={t('requests.filters.state')} value={state} onChange={setState}>
              <option value="">{t('requests.filters.allStates')}</option>
              {REQUEST_STATES.map((s) => (
                <option key={s} value={s}>
                  {stateLabel(requestState(s))}
                </option>
              ))}
            </FilterSelect>
            <Segmented<string>
              label={t('requests.filters.category')}
              options={['', ...CATEGORIES]}
              value={category}
              onChange={setCategory}
              render={(c) =>
                c ? categoryLabel(c as CategoryName) : t('requests.filters.allCategories')
              }
            />
            <FilterSelect
              label={t('requests.filters.memberLabel')}
              value={member ? String(member) : ''}
              onChange={(v) => setMember(Number(v) || 0)}
            >
              <option value="">{t('requests.filters.allMembers')}</option>
              {member > 0 && !members.has(member) && <option value={member}>#{member}</option>}
              {[...members].map(([id, name]) => (
                <option key={id} value={id}>
                  {name}
                </option>
              ))}
            </FilterSelect>
          </>
        }
        empty={
          state || category || member || query
            ? t('requests.noMatchesDescription')
            : t('requests.emptyDescription')
        }
        selection={{
          selected,
          onChange: setSelected,
          // Neutral private rows are never decided blind (PRIV-08).
          isSelectable: (r) => !r.redacted && (pending(r) || r.canRevoke),
          rowLabel: (r) => t('requests.select', { title: r.title }),
          allLabel: t('requests.selectAll'),
          bar: (chosen) => {
            const waiting = chosen.filter(pending)
            const open = chosen.filter((r) => r.canRevoke)
            return (
              <>
                {waiting.length > 0 && (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={approve.isPending}
                    onClick={() =>
                      approve.mutate(waiting.map(refOf), {
                        onSuccess: (result) => {
                          setSelected(new Set())
                          feedback(waiting, result)
                        },
                        onError: (e) => toast.error(errorMessage(e)),
                      })
                    }
                  >
                    <CheckIcon aria-hidden className="text-ok" />
                    {t('requests.approveSelected', { count: waiting.length })}
                  </Button>
                )}
                {waiting.length > 0 && (
                  <Button size="sm" variant="ghost" onClick={() => setDeclining(waiting)}>
                    <XIcon aria-hidden />
                    {t('requests.declineSelected', { count: waiting.length })}
                  </Button>
                )}
                {open.length > 0 && (
                  <Button size="sm" variant="ghost" onClick={() => setRevoking(open)}>
                    <BanIcon aria-hidden />
                    {t('requests.revokeSelectedCount', { count: open.length })}
                  </Button>
                )}
                <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>
                  {t('requests.clearSelection')}
                </Button>
              </>
            )
          },
        }}
      />

      <Sheet open={open > 0} onOpenChange={(o) => !o && setOpen(0)}>
        <SheetContent side="right" className="w-full max-w-xl overflow-y-auto sm:max-w-xl">
          {open > 0 && (
            <RequestDetailPanel
              id={open}
              onRevoke={(request) => setRevoking([request])}
              onDecline={(request) => setDeclining([request])}
            />
          )}
        </SheetContent>
      </Sheet>
      <ReasonDialog
        action="revoke"
        requests={revoking}
        onDone={(done) => {
          setRevoking(null)
          if (done) setSelected(new Set())
        }}
      />
      <ReasonDialog
        action="reject"
        requests={declining}
        onDone={(done) => {
          setDeclining(null)
          if (done) setSelected(new Set())
        }}
      />
    </>
  )
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-text-3">{label}</dt>
      <dd className="text-text">{children}</dd>
    </>
  )
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2">
      <h3 className="type-data text-xs tracking-wide text-text-3 uppercase">{title}</h3>
      {children}
    </section>
  )
}

function Reason({ tone, children }: { tone: 'bad' | 'warn'; children: ReactNode }) {
  return (
    <p
      className={cn(
        'chip flex items-start gap-2 rounded-card px-3 py-2 text-sm break-words',
        tone === 'bad' ? '[--chip:var(--color-bad)]' : '[--chip:var(--color-warn)]',
      )}
    >
      <CircleAlertIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
      <span>{children}</span>
    </p>
  )
}

/**
 * One request with an action bar per state (ADM-04): Approve and Decline while it waits, Submit
 * again for a failed download, the held reason with the way to resume, a warning for a stuck
 * download, Revoke while something is open. One gold action at most (the approval).
 */
function RequestDetailPanel({
  id,
  onRevoke,
  onDecline,
}: {
  id: number
  onRevoke: (request: AdminRequest) => void
  onDecline: (request: AdminRequest) => void
}) {
  const { t } = useTranslation(['admin', 'common'])
  const journeyTitle = useJourneyTitle()
  const categoryLabel = useCategoryLabel()
  const detail = useAdminRequest(id)
  const approve = useApproveRequests()
  const retry = useRetryAcquisition()
  const feedback = useApprovalFeedback()
  const errorMessage = useApiErrorMessage()
  const [error, setError] = useState<string | null>(null)

  if (!detail.data) {
    return (
      <div className="p-6">
        <SheetTitle className="type-display text-xl font-bold">
          {t('requests.detail.title', { id })}
        </SheetTitle>
        <SheetDescription className="mt-2 text-text-3">
          {detail.isError ? t('requests.loadFailed') : t('common:status.loading')}
        </SheetDescription>
      </div>
    )
  }
  const { request, events } = detail.data
  const waiting = pending(request) && !request.redacted
  const failed = request.items.filter((i) => i.state === 'failed' && i.acquisitionId != null)
  const held = request.items.some((i) => i.journey.steps.some((s) => s.step === 'held'))
  const source = request.items.find((i) => i.journey.source)?.journey.source ?? '–'
  const stuck = request.items.filter((i) => i.sourceMessage && i.state === 'in_progress')

  return (
    <div className="flex flex-col gap-6 p-6 pr-12">
      <div className="flex gap-4">
        <PosterThumb file={request.poster} title={request.title} category={request.category} />
        <div className="flex flex-col gap-2">
          <SheetTitle className="type-display text-xl font-bold">
            {request.title}
            {request.year != null && (
              <span className="type-data ml-2 text-base font-normal text-text-3">
                {request.year}
              </span>
            )}
          </SheetTitle>
          <SheetDescription asChild>
            <div>
              <RequestStateChip
                units={request.items}
                state={request.state}
                stage={request.stage}
                arrivingIn={request.arrivingIn}
                admin
              />
            </div>
          </SheetDescription>
        </div>
      </div>

      {/* The action bar per state (ADM-04). */}
      <Section title={t('requests.detail.actions')}>
        {held && (
          <Reason tone="warn">
            <span className="flex flex-col items-start gap-1.5">
              <span className="flex items-center gap-1.5">
                <PauseIcon aria-hidden className="size-3.5" />
                {t('requests.detail.held', { source })}
              </span>
              <Link to="/admin/inbox" className="font-semibold underline underline-offset-4">
                {t('requests.detail.heldOpen')}
              </Link>
            </span>
          </Reason>
        )}
        {stuck.map((item) => (
          <Reason key={item.id} tone="warn">
            {t('requests.detail.stuck', { message: item.sourceMessage })}
          </Reason>
        ))}
        <div className="flex flex-wrap gap-2">
          {waiting && (
            <Button
              variant="primary"
              disabled={approve.isPending}
              onClick={() => {
                setError(null)
                approve.mutate([refOf(request)], {
                  onSuccess: (result) => feedback([request], result),
                  onError: (e) => setError(errorMessage(e)),
                })
              }}
            >
              <CheckIcon aria-hidden />
              {t('inbox.approve')}
            </Button>
          )}
          {waiting && (
            <Button variant="ghost" onClick={() => onDecline(request)}>
              <XIcon aria-hidden />
              {t('inbox.decline')}
            </Button>
          )}
          {request.redacted && pending(request) && (
            <Button asChild variant="ghost">
              <Link to={inboxLink(`approval:${request.id}`)}>{t('inbox.private.show')}</Link>
            </Button>
          )}
          {failed.map((item) => (
            <Button
              key={item.id}
              variant="secondary"
              disabled={retry.isPending}
              onClick={() => {
                setError(null)
                retry.mutate(item.acquisitionId as number, {
                  onSuccess: () =>
                    toast(
                      request.category === 'adult'
                        ? t('inbox.failed.submittedPrivate')
                        : t('inbox.failed.submitted', { title: requestLabel(request) }),
                    ),
                  onError: (e) => setError(errorMessage(e)),
                })
              }}
            >
              <RotateCwIcon aria-hidden />
              {t('requests.detail.submitAgain')}
              {item.season != null && ` ${seasonLabel([item.season])}`}
              {item.album && ` · ${item.album.title}`}
            </Button>
          ))}
          {request.canRevoke && (
            <Button variant="ghost" onClick={() => onRevoke(request)}>
              <BanIcon aria-hidden />
              {t('requests.revoke')}
            </Button>
          )}
        </div>
        {error && (
          <p role="alert" className="text-sm text-bad">
            {t('inbox.rowError', { message: error })}
          </p>
        )}
      </Section>

      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
        <Fact label={t('requests.detail.member')}>
          {request.redacted ? (
            request.member.displayName
          ) : (
            <Link to={`/admin/people/members/${request.member.id}`} className="hover:underline">
              {request.member.displayName}{' '}
              <span className="type-data text-text-3">{request.member.username}</span>
            </Link>
          )}
        </Fact>
        <Fact label={t('requests.detail.category')}>
          <span className="inline-flex items-center gap-2">
            <CategoryDot category={request.category} />
            {categoryLabel(request.category)}
          </span>
        </Fact>
        <Fact label={t('requests.detail.requested')}>{formatDateTime(request.createdAt)}</Fact>
        <Fact label={t('requests.detail.updated')}>{formatDateTime(request.updatedAt)}</Fact>
        {request.kind === 'series' && (
          <Fact label={t('requests.detail.follow')}>
            {request.follow ? t('requests.detail.followYes') : t('requests.detail.followNo')}
          </Fact>
        )}
      </dl>

      {request.rejectReason && (
        <Section title={t('requests.detail.rejectReason')}>
          <p className="rounded-card border border-line px-3 py-2 text-sm text-text">
            {request.rejectReason}
          </p>
        </Section>
      )}
      {request.revokeReason && (
        <Section title={t('requests.detail.revokeReason')}>
          <Reason tone="bad">{request.revokeReason}</Reason>
        </Section>
      )}
      {request.hidden && (
        <p className="flex items-center gap-2 text-sm text-text-3">
          <EyeOffIcon aria-hidden className="size-3.5" strokeWidth={1.75} />
          {t('requests.detail.hiddenByMember')}
        </p>
      )}

      <Section title={t('requests.detail.items')}>
        <ul className="flex flex-col divide-y divide-line rounded-card border border-line">
          {request.items.map((item) => (
            <li key={item.id} className="flex flex-col gap-1.5 px-3 py-2.5">
              <div className="flex flex-wrap items-center justify-between gap-3">
                {item.album ? (
                  <span className="font-medium text-text">{item.album.title}</span>
                ) : (
                  <span className="type-data font-medium text-text">
                    {item.season != null
                      ? t('requests.detail.season', { n: item.season })
                      : t('requests.detail.movie')}
                  </span>
                )}
                <span className="flex items-center gap-2">
                  {item.state === 'in_progress' && !item.stage && item.progress != null && (
                    <ProgressBar value={item.progress} />
                  )}
                  <RequestStateChip
                    units={[item]}
                    state={item.state}
                    stage={item.stage}
                    arrivingIn={item.arrivingIn}
                    admin
                  />
                </span>
              </div>
              {item.lastError && item.state === 'failed' && (
                <Reason tone="bad">{item.lastError}</Reason>
              )}
              {item.acquisitionId != null && (
                <Link
                  to={`/admin/downloads?open=${item.acquisitionId}`}
                  className="self-start text-sm text-text-2 underline-offset-4 hover:underline"
                >
                  {t('requests.detail.openDownload')}
                </Link>
              )}
            </li>
          ))}
        </ul>
      </Section>

      {!request.redacted && (
        <Section title={t('requests.detail.journey')}>
          <div className="rounded-card border border-line p-4">
            <JourneyCard
              title={journeyTitle(request.title, request.kind)}
              units={request.items}
              albums={albumTitles(request.items)}
              admin
            />
          </div>
        </Section>
      )}

      <Section title={t('requests.detail.history')}>
        <ol className="flex flex-col gap-3 border-l border-line pl-4">
          {events.map((event, index) => (
            <li key={index} className="relative flex flex-col gap-0.5 text-sm">
              <span
                aria-hidden
                className="absolute top-1.5 -left-[1.3rem] size-2 rounded-full bg-text-3"
              />
              <span className="text-text">
                {event.fromState
                  ? t('requests.detail.transition', {
                      from: stateLabel(requestState(event.fromState)),
                      to: stateLabel(requestState(event.toState)),
                    })
                  : t('requests.detail.created', {
                      state: stateLabel(requestState(event.toState)),
                    })}
              </span>
              <span className="type-data text-xs text-text-3">
                {formatDateTime(event.at)} · {t(`requests.trigger.${event.trigger}`)}
                {event.actor ? ` · ${event.actor}` : ''}
              </span>
              {event.detail && <span className="text-text-2">{event.detail}</span>}
            </li>
          ))}
        </ol>
      </Section>
    </div>
  )
}

/** Downloads that need someone: held at a paused source, failed, or stuck. */
const SHOW = ['all', 'attention', 'held', 'failed', 'stuck'] as const
type Show = (typeof SHOW)[number]

function isStuck(a: Acquisition): boolean {
  return a.sourceMessage != null && (a.state === 'downloading' || a.state === 'submitted')
}

function shows(a: Acquisition, show: Show): boolean {
  switch (show) {
    case 'all':
      return true
    case 'held':
      return a.held
    case 'failed':
      return a.state === 'failed'
    case 'stuck':
      return isStuck(a)
    case 'attention':
      return a.held || a.state === 'failed' || isStuck(a)
  }
}

function AcquisitionsView() {
  const { t } = useTranslation(['admin', 'common'])
  const errorMessage = useApiErrorMessage()
  const [state, setState] = useUrlState<string>('state', z.enum(ACQUISITION_STATES), '')
  const [show, setShow] = useUrlState<Show>('show', z.enum(SHOW), 'all')
  const [sort, setSort] = useUrlSort()
  const [open, setOpen] = useUrlState('open', z.coerce.number().int().positive(), 0)
  const [query, setQuery] = useSearchText()
  const acquisitions = useAcquisitions((state || undefined) as AcquisitionState | undefined)
  const list = acquisitions.data?.filter((a) => shows(a, show))
  const selected = acquisitions.data?.find((a) => a.id === open) ?? null

  const columns: Column<Acquisition>[] = [
    {
      id: 'title',
      header: t('acquisitions.columns.title'),
      sortValue: (a) => a.title,
      className: 'min-w-64',
      cell: (a) => (
        <TitleCell
          poster={a.poster}
          title={a.title}
          year={a.year}
          category={a.category}
          extra={a.album?.title ?? seasonLabel([a.season])}
          onOpen={() => setOpen(a.id, { push: true })}
          openLabel={t('acquisitions.openDetails', { title: a.title })}
        />
      ),
    },
    {
      id: 'source',
      header: t('acquisitions.columns.source'),
      sortValue: (a) => a.serviceName ?? '',
      className: 'w-40',
      cell: (a) => (
        <span className="text-sm text-text-2">
          {a.serviceName ?? t('acquisitions.sourceRemoved')}
          <span className="type-data block text-xs text-text-3">
            {t('acquisitions.shared', { count: a.requests })}
          </span>
        </span>
      ),
    },
    {
      id: 'state',
      header: t('acquisitions.columns.state'),
      className: 'w-56',
      cell: (a) => (
        <div className="flex flex-wrap items-center gap-2">
          <AcquisitionStateChip
            units={[a]}
            state={a.state}
            held={a.held}
            stage={a.stage}
            source={a.serviceName}
            arrivingIn={a.arrivingIn}
          />
          {a.state === 'downloading' && !a.stage && a.progress != null && (
            <ProgressBar value={a.progress} />
          )}
        </div>
      ),
    },
    {
      id: 'note',
      header: t('acquisitions.columns.note'),
      className: 'max-w-72',
      cell: (a) => <AcquisitionNote acquisition={a} />,
    },
    {
      id: 'updated',
      header: t('acquisitions.columns.updated'),
      sortValue: (a) => a.updatedAt,
      align: 'end',
      className: 'w-32',
      cell: (a) => (
        <time
          dateTime={a.updatedAt}
          title={formatDateTime(a.updatedAt)}
          className="type-data text-sm text-text-2"
        >
          {formatRelative(a.updatedAt)}
        </time>
      ),
    },
  ]

  return (
    <>
      <DataTable<Acquisition>
        label={t('requests.downloadsTitle')}
        columns={columns}
        rows={list}
        rowKey={(a) => a.id}
        loading={acquisitions.isPending}
        error={acquisitions.isError ? errorMessage(acquisitions.error) : null}
        onRetry={() => void acquisitions.refetch()}
        sort={sort}
        onSortChange={setSort}
        search={{
          label: t('acquisitions.filters.search'),
          value: query,
          onChange: setQuery,
          matches: (a, q) => {
            const needle = q.toLowerCase()
            return [a.title, a.album?.title ?? '', a.serviceName ?? ''].some((v) =>
              v.toLowerCase().includes(needle),
            )
          },
        }}
        filters={
          <>
            <Segmented<Show>
              label={t('acquisitions.filters.attention')}
              options={SHOW}
              value={show}
              onChange={setShow}
              render={(s) => t(`acquisitions.filters.${s}`)}
            />
            <FilterSelect label={t('requests.filters.state')} value={state} onChange={setState}>
              <option value="">{t('requests.filters.allStates')}</option>
              {ACQUISITION_STATES.map((s) => (
                <option key={s} value={s}>
                  {stateLabel(acquisitionState(s))}
                </option>
              ))}
            </FilterSelect>
          </>
        }
        empty={
          show !== 'all' || state || query
            ? t('requests.noMatchesDescription')
            : t('acquisitions.emptyDescription')
        }
      />

      <Sheet open={selected !== null} onOpenChange={(o) => !o && setOpen(0)}>
        <SheetContent side="right" className="w-full max-w-xl overflow-y-auto sm:max-w-xl">
          {selected && <AcquisitionDetailPanel acquisition={selected} />}
        </SheetContent>
      </Sheet>
    </>
  )
}

/** The one line that explains an acquisition's situation, if any. */
function AcquisitionNote({ acquisition }: { acquisition: Acquisition }) {
  const { t } = useTranslation('admin')
  const source = acquisition.serviceName ?? '–'
  if (acquisition.stopPending) {
    return (
      <span className="line-clamp-2 text-warn">
        {t(acquisition.stopHeld ? 'acquisitions.stopHeld' : 'acquisitions.stopPending', {
          name: source,
        })}
      </span>
    )
  }
  if (acquisition.unwanted) {
    return <span className="line-clamp-2 text-text-2">{t('acquisitions.unwanted')}</span>
  }
  if (acquisition.held) {
    return (
      <span className="line-clamp-2 text-warn">
        {t('acquisitions.held', { name: acquisition.serviceName ?? '–' })}
      </span>
    )
  }
  if (acquisition.state === 'failed' && acquisition.lastError) {
    return <span className="line-clamp-2 text-bad">{acquisition.lastError}</span>
  }
  if (acquisition.state === 'pending' && acquisition.lastError && acquisition.nextAttemptAt) {
    return (
      <span className="line-clamp-2 text-warn">
        {t('acquisitions.retrying', { when: formatRelative(acquisition.nextAttemptAt) })}
      </span>
    )
  }
  if (acquisition.sourceMessage) {
    return <span className="line-clamp-2 text-warn">{acquisition.sourceMessage}</span>
  }
  return <span className="text-text-3">—</span>
}

function AcquisitionDetailPanel({ acquisition }: { acquisition: Acquisition }) {
  const { t } = useTranslation(['admin', 'common'])
  const journeyTitle = useJourneyTitle()
  const retry = useRetryAcquisition()
  const errorMessage = useApiErrorMessage()
  const [error, setError] = useState<string | null>(null)
  const optional = (iso?: string | null) => (iso ? formatDateTime(iso) : '—')

  return (
    <div className="flex flex-col gap-6 p-6 pr-12">
      <div className="flex gap-4">
        <PosterThumb
          file={acquisition.poster}
          title={acquisition.title}
          category={acquisition.category}
        />
        <div className="flex flex-col gap-2">
          <SheetTitle className="type-display text-xl font-bold">
            {acquisition.title}
            {acquisition.season != null && (
              <span className="type-data ml-2 text-base font-normal text-text-3">
                {seasonLabel([acquisition.season])}
              </span>
            )}
            {acquisition.album && (
              <span className="ml-2 text-base font-normal text-text-2">
                {acquisition.album.title}
              </span>
            )}
          </SheetTitle>
          <SheetDescription asChild>
            <div className="flex items-center gap-2">
              <AcquisitionStateChip
                units={[acquisition]}
                state={acquisition.state}
                held={acquisition.held}
                stage={acquisition.stage}
                source={acquisition.serviceName}
                arrivingIn={acquisition.arrivingIn}
              />
              {acquisition.state === 'downloading' &&
                !acquisition.stage &&
                acquisition.progress != null && <ProgressBar value={acquisition.progress} />}
            </div>
          </SheetDescription>
        </div>
      </div>

      {acquisition.state === 'failed' && (
        <Section title={t('acquisitions.detail.failure')}>
          <Reason tone="bad">{acquisition.lastError}</Reason>
          {acquisition.errorCode && (
            <p className="type-data text-xs text-text-3">{acquisition.errorCode}</p>
          )}
          <div>
            <Button
              variant="primary"
              disabled={retry.isPending}
              onClick={() => {
                setError(null)
                retry.mutate(acquisition.id, {
                  onSuccess: () => toast(t('acquisitions.retried')),
                  onError: (e) => setError(errorMessage(e)),
                })
              }}
            >
              <RotateCwIcon aria-hidden />
              {t('acquisitions.retry')}
            </Button>
          </div>
          {error && (
            <p role="alert" className="text-sm text-bad">
              {t('inbox.rowError', { message: error })}
            </p>
          )}
        </Section>
      )}
      {(acquisition.unwanted || acquisition.stopPending || acquisition.stoppedAt) && (
        <Section title={t('acquisitions.detail.unwanted')}>
          {acquisition.unwanted && (
            <p className="text-sm text-text-2">{t('acquisitions.unwanted')}</p>
          )}
          {acquisition.stopPending && (
            <Reason tone="warn">
              {t(acquisition.stopHeld ? 'acquisitions.stopHeld' : 'acquisitions.stopPending', {
                name: acquisition.serviceName ?? '–',
              })}
            </Reason>
          )}
          {acquisition.stoppedAt && (
            <p className="text-sm text-text-2">
              {t('acquisitions.stopped', { when: formatRelative(acquisition.stoppedAt) })}
            </p>
          )}
        </Section>
      )}
      {acquisition.held && (
        <Section title={t('acquisitions.detail.held')}>
          <Reason tone="warn">
            {t('acquisitions.held', { name: acquisition.serviceName ?? '–' })}
          </Reason>
          {acquisition.serviceId != null && (
            <Link
              to={inboxLink(`held:${acquisition.serviceId}`)}
              className="self-start text-sm font-semibold text-text underline underline-offset-4"
            >
              {t('requests.detail.heldOpen')}
            </Link>
          )}
        </Section>
      )}
      {acquisition.sourceMessage && (
        <Section title={t('acquisitions.detail.sourceMessage')}>
          <Reason tone="warn">{acquisition.sourceMessage}</Reason>
        </Section>
      )}

      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
        <Fact label={t('acquisitions.columns.source')}>
          {acquisition.serviceName ?? t('acquisitions.sourceRemoved')}
        </Fact>
        <Fact label={t('acquisitions.detail.shared')}>
          {t('acquisitions.shared', { count: acquisition.requests })}
        </Fact>
        <Fact label={t('acquisitions.detail.attempts')}>
          <span className="type-data">{acquisition.submitAttempts}</span>
        </Fact>
        {acquisition.state === 'pending' && acquisition.nextAttemptAt && (
          <Fact label={t('acquisitions.detail.nextAttempt')}>
            {formatDateTime(acquisition.nextAttemptAt)}
          </Fact>
        )}
        <Fact label={t('acquisitions.detail.created')}>
          {formatDateTime(acquisition.createdAt)}
        </Fact>
        <Fact label={t('acquisitions.detail.submitted')}>{optional(acquisition.submittedAt)}</Fact>
        <Fact label={t('acquisitions.detail.synced')}>{optional(acquisition.syncedAt)}</Fact>
        <Fact label={t('acquisitions.detail.available')}>{optional(acquisition.availableAt)}</Fact>
      </dl>

      {!acquisition.redacted && (
        <div className="rounded-card border border-line p-4">
          <JourneyCard
            title={journeyTitle(
              acquisition.title,
              acquisitionKind(acquisition),
              acquisition.season,
              acquisition.album?.title,
            )}
            units={[acquisition]}
            admin
          />
        </div>
      )}
    </div>
  )
}
