import {
  ArrowLeftIcon,
  CheckIcon,
  CircleCheckIcon,
  InboxIcon,
  RotateCwIcon,
  XIcon,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { z } from 'zod'

import {
  type AttentionItem,
  type AttentionSection,
  useAttention,
  useDropAttentionItems,
} from '@/api/attention'
import { useApiErrorMessage } from '@/api/errors'
import { type AdminRequest, refOf, useApproveRequests } from '@/api/requests'
import { CategoryDot } from '@/components/CategoryDot'
import { EmptyState } from '@/components/EmptyState'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { JourneyCard } from '@/components/StatusJourney'
import { useMediaQuery, WIDE } from '@/components/shell/useSidebar'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { bySection, inboxLink, nextAfter, SECTIONS } from '@/lib/attention'
import { formatDateTime, formatTime } from '@/lib/format'
import { albumTitles, useJourneyTitle } from '@/lib/journey'
import { focusElement } from '@/lib/returnFocus'
import { useUrlState } from '@/lib/urlState'
import { cn } from '@/lib/utils'

import { useApprovalFeedback } from './approvals'
import { ItemActions, ItemBody, ItemThumb, ItemWho, KindChip } from './inbox/InboxItem'
import { useCategoryLabel, useItemTitle } from './inbox/items'
import { ReasonDialog } from './ReasonDialog'

/*
 * The admin Inbox (spec 0008 F9, Q9 A + C's detail panel): everything that needs an admin in four
 * sections, Decide · Fix · Paused · Setup, each with its count and empty ones folded into one
 * "nothing waiting" line. Rows act in place without gold (ADM-16); "Open" and bell links show the
 * item's detail panel (`?open=`) with exactly one gold action. At ≥ 1280 px the panel sits
 * beside the sections, below it replaces them with "Back to inbox". The one count comes from
 * the attention list and drops at once when something is resolved (ADM-23).
 */

/** Where focus goes once the page re-rendered after an action. */
type FocusTarget =
  | { kind: 'row'; id: string }
  | { kind: 'open'; id: string }
  | { kind: 'panel' }
  | { kind: 'heading' }

export default function InboxPage() {
  const { t } = useTranslation(['admin', 'common'])
  const attention = useAttention()
  const errorMessage = useApiErrorMessage()
  const [open, setOpen] = useUrlState('open', z.string(), '')
  const wide = useMediaQuery(WIDE)
  const drop = useDropAttentionItems()
  const heading = useRef<HTMLHeadingElement>(null)
  // Set by an action, used after the next render (no state: nothing to render for it).
  const pendingFocus = useRef<FocusTarget | null>(null)
  const items = attention.data?.items ?? []
  const openItem = items.find((i) => i.id === open) ?? null
  const total = attention.data?.total ?? 0

  useEffect(() => {
    const focus = pendingFocus.current
    if (!focus) return
    pendingFocus.current = null
    const row = (id: string, what: string) =>
      document.querySelector<HTMLElement>(`[data-item="${CSS.escape(id)}"] ${what}`)
    const target =
      focus.kind === 'heading'
        ? heading.current
        : focus.kind === 'panel'
          ? document.getElementById('inbox-detail-heading')
          : focus.kind === 'open'
            ? row(focus.id, '[data-open]')
            : row(focus.id, ':is(button:not([disabled]), a[href], input)')
    if (target) focusElement(target)
    else if (heading.current) focusElement(heading.current)
  })

  /**
   * Items are done (one, or a bulk action's): out of the list at once, selection and focus move
   * on past all of them (Q9 C, F3).
   */
  const resolvedMany = (ids: string[]) => {
    if (ids.length === 0) return
    const gone = new Set(ids)
    const from = gone.has(open) ? open : ids[0]
    const next = nextAfter(items, from, gone)
    drop(ids)
    if (gone.has(open)) {
      setOpen(next ?? '')
      pendingFocus.current = next ? { kind: 'panel' } : { kind: 'heading' }
    } else {
      pendingFocus.current = next ? { kind: 'row', id: next } : { kind: 'heading' }
    }
  }
  const resolved = (id: string) => resolvedMany([id])

  const closePanel = () => {
    const id = open
    setOpen('')
    pendingFocus.current = { kind: 'open', id }
  }

  const header = (
    <div className="mb-2 flex flex-wrap items-end gap-x-5 gap-y-3">
      <div className="flex flex-col gap-1">
        <h1
          ref={heading}
          tabIndex={-1}
          className="type-display text-2xl font-extrabold text-text outline-none"
        >
          {t('inbox.title')}
        </h1>
        {attention.data && (
          <p className="text-base text-text-2" aria-live="polite">
            {total > 0 ? t('inbox.summary', { count: total }) : t('inbox.summaryNone')}
          </p>
        )}
      </div>
      <span className="grow" />
      {attention.dataUpdatedAt > 0 && (
        <span className="flex items-center gap-2 text-sm text-text-3">
          <span>
            {t('inbox.updated', {
              time: formatTime(new Date(attention.dataUpdatedAt).toISOString()),
            })}
          </span>
          <Button
            variant="ghost"
            size="icon"
            className="size-9"
            aria-label={t('inbox.refresh')}
            disabled={attention.isFetching}
            onClick={() => void attention.refetch()}
          >
            <RotateCwIcon aria-hidden />
          </Button>
        </span>
      )}
    </div>
  )

  if (attention.isPending) {
    return (
      <>
        {header}
        <div aria-busy className="mt-6 flex flex-col gap-3">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-20 w-full rounded-panel" />
          ))}
        </div>
      </>
    )
  }
  if (attention.isError) {
    return (
      <>
        {header}
        <QueryError
          className="mt-6"
          message={t('inbox.loadFailed')}
          detail={errorMessage(attention.error)}
          onRetry={() => void attention.refetch()}
        />
      </>
    )
  }

  const panel = openItem && (
    <DetailPanel item={openItem} narrow={!wide} onBack={closePanel} onResolved={resolved} />
  )
  const gone = open !== '' && !openItem
  const sections = (
    <div className="flex flex-col">
      {gone && (
        <p role="status" className="mt-4 text-sm text-text-2">
          {t('inbox.openGone')}
        </p>
      )}
      {total === 0 ? (
        <EmptyState
          className="mt-6"
          icon={InboxIcon}
          title={t('inbox.allClearTitle')}
          description={t('inbox.allClearDescription')}
          action={
            <Button asChild variant="ghost">
              <Link to="/admin/requests">{t('inbox.openRequests')}</Link>
            </Button>
          }
        />
      ) : (
        <Sections items={items} openId={open} onResolved={resolved} onResolvedMany={resolvedMany} />
      )}
    </div>
  )

  return (
    <>
      {header}
      {openItem && !wide ? (
        panel
      ) : openItem ? (
        <div className="grid grid-cols-[minmax(0,1fr)_minmax(22rem,26rem)] items-start gap-6">
          {sections}
          <div className="sticky top-4 mt-6">{panel}</div>
        </div>
      ) : (
        sections
      )}
    </>
  )
}

function Sections({
  items,
  openId,
  onResolved,
  onResolvedMany,
}: {
  items: AttentionItem[]
  openId: string
  onResolved: (id: string) => void
  onResolvedMany: (ids: string[]) => void
}) {
  const groups = bySection(items)
  return (
    <>
      {SECTIONS.map((section) => (
        <Section
          key={section}
          section={section}
          items={groups[section]}
          openId={openId}
          onResolved={onResolved}
          onResolvedMany={onResolvedMany}
        />
      ))}
    </>
  )
}

function Section({
  section,
  items,
  openId,
  onResolved,
  onResolvedMany,
}: {
  section: AttentionSection
  items: AttentionItem[]
  openId: string
  onResolved: (id: string) => void
  onResolvedMany: (ids: string[]) => void
}) {
  const { t } = useTranslation('admin')
  const id = `inbox-${section}`
  const [selected, setSelected] = useState<Set<string>>(new Set())
  if (items.length === 0) {
    return (
      <section
        aria-labelledby={id}
        className="mt-3.5 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-panel border border-dashed border-line px-4 py-2.5"
      >
        <h2 id={id} className="flex items-center gap-2 text-base font-semibold text-text-2">
          <CircleCheckIcon aria-hidden className="size-4 text-ok" />
          {t(`inbox.sections.${section}`)}
        </h2>
        <p className="text-sm text-text-3">{t('inbox.nothingWaiting')}</p>
      </section>
    )
  }
  // Only approvals the admin can see are chosen together (never blind, PRIV-08).
  const selectable =
    section === 'decide' ? items.filter((i) => i.request && !i.request.redacted) : []
  const chosen = selectable.filter((i) => selected.has(i.id))
  return (
    <section aria-labelledby={id} className="mt-7">
      <div className="mb-2.5 flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h2 id={id} className="type-display flex items-center gap-2.5 text-xl font-bold text-text">
          {t(`inbox.sections.${section}`)}
          <span
            aria-hidden
            className="type-data rounded-full border border-line bg-night-2 px-2 py-0.5 text-xs font-semibold text-text"
          >
            {items.length}
          </span>
          <span className="sr-only">{t('inbox.itemsCount', { count: items.length })}</span>
        </h2>
        <p className="text-sm text-text-3">{t(`inbox.sectionHint.${section}`)}</p>
      </div>
      {chosen.length > 0 && (
        <BulkBar
          items={chosen}
          onClear={() => setSelected(new Set())}
          onResolved={(ids) => {
            setSelected(new Set())
            onResolvedMany(ids)
          }}
        />
      )}
      <ul className="flex flex-col divide-y divide-line rounded-panel border border-line bg-night-1 max-md:gap-3 max-md:divide-y-0 max-md:border-0 max-md:bg-transparent">
        {items.map((item) => (
          <Row
            key={item.id}
            item={item}
            current={item.id === openId}
            selectable={selectable.includes(item)}
            selected={selected.has(item.id)}
            onSelect={(on) =>
              setSelected((current) => {
                const next = new Set(current)
                if (on) next.add(item.id)
                else next.delete(item.id)
                return next
              })
            }
            onResolved={(rid) => {
              setSelected((current) => {
                const next = new Set(current)
                next.delete(rid)
                return next
              })
              onResolved(rid)
            }}
          />
        ))}
      </ul>
    </section>
  )
}

/** The bar over the Decide rows while some are chosen; it stays in view (ADM-31). */
function BulkBar({
  items,
  onClear,
  onResolved,
}: {
  items: AttentionItem[]
  onClear: () => void
  onResolved: (ids: string[]) => void
}) {
  const { t } = useTranslation('admin')
  const approve = useApproveRequests()
  const feedback = useApprovalFeedback()
  const errorMessage = useApiErrorMessage()
  const [error, setError] = useState<string | null>(null)
  const [declining, setDeclining] = useState<AdminRequest[] | null>(null)
  const requests = items.flatMap((i) => (i.request ? [i.request] : []))
  const idOf = (requestId: number) => `approval:${requestId}`
  return (
    <div
      role="region"
      aria-label={t('inbox.selected', { count: items.length })}
      className="sticky top-14 z-20 mb-3 flex flex-wrap items-center gap-3 rounded-panel border border-line bg-night-2 px-4 py-2.5 lg:top-2"
    >
      <span className="text-sm font-medium text-text" aria-live="polite">
        {t('inbox.selected', { count: items.length })}
      </span>
      <div className="ml-auto flex flex-wrap gap-2 max-md:grid max-md:w-full max-md:grid-cols-2 max-md:[&>*]:h-11">
        <Button
          size="sm"
          variant="ghost"
          disabled={approve.isPending}
          onClick={() => {
            setError(null)
            approve.mutate(requests.map(refOf), {
              onSuccess: (result) => {
                onResolved([...result.decided, ...result.skipped].map(idOf))
                feedback(requests, result)
              },
              onError: (e) => setError(errorMessage(e)),
            })
          }}
        >
          <CheckIcon aria-hidden className="text-ok" />
          {t('inbox.approveSelected')}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setDeclining(requests)}>
          <XIcon aria-hidden />
          {t('inbox.declineSelected')}
        </Button>
        <Button size="sm" variant="ghost" className="max-md:col-span-2" onClick={onClear}>
          {t('inbox.clearSelection')}
        </Button>
      </div>
      {error && (
        <p role="alert" className="basis-full text-sm text-bad">
          {t('inbox.rowError', { message: error })}
        </p>
      )}
      <ReasonDialog
        action="reject"
        requests={declining}
        onDone={(declined) => {
          const done = declining ?? []
          setDeclining(null)
          if (declined) onResolved(done.map((r) => idOf(r.id)))
        }}
      />
    </div>
  )
}

function Row({
  item,
  current,
  selectable,
  selected,
  onSelect,
  onResolved,
}: {
  item: AttentionItem
  current: boolean
  selectable: boolean
  selected: boolean
  onSelect: (on: boolean) => void
  onResolved: (id: string) => void
}) {
  const { t } = useTranslation('admin')
  const title = useItemTitle()(item)
  const openLink = (
    <Button asChild size="sm" variant="ghost">
      <Link
        to={inboxLink(item.id)}
        data-open
        aria-label={t('inbox.openLabel', { title })}
        aria-current={current || undefined}
      >
        {t('inbox.open')}
      </Link>
    </Button>
  )
  return (
    <li
      data-item={item.id}
      className={cn(
        // Thumb, who-and-when and actions are centred against the row; only the text block
        // keeps its lines top-aligned inside (design README, "Rows").
        'flex flex-wrap items-center gap-x-4 gap-y-3 px-4 py-3.5 md:[&>[data-actions]]:ml-auto md:[&>[data-actions]]:min-w-80 md:[&>[data-actions]]:justify-end',
        'max-md:rounded-panel max-md:border max-md:border-line max-md:bg-night-1 max-md:p-4',
        (current || selected) && 'bg-night-2/50',
      )}
    >
      <div className="flex shrink-0 items-center gap-3">
        {selectable && (
          <Checkbox
            aria-label={t('inbox.select', { title })}
            checked={selected}
            onCheckedChange={(on) => onSelect(on === true)}
          />
        )}
        <ItemThumb item={item} />
      </div>
      <div className="min-w-0 flex-1 basis-48">
        <ItemHeading item={item} title={title} />
        <div className="mt-1">
          <ItemBody item={item} />
        </div>
      </div>
      <div className="w-36 shrink-0 max-md:w-auto max-md:basis-full">
        <ItemWho item={item} />
      </div>
      <ItemActions item={item} onResolved={onResolved} openLink={openLink} />
    </li>
  )
}

function ItemHeading({ item, title }: { item: AttentionItem; title: string }) {
  const category = useCategoryLabel()
  const c = item.request?.category ?? item.acquisition?.category ?? item.category?.category
  return (
    <p className="flex flex-wrap items-center gap-x-2.5 gap-y-1">
      <span className="font-semibold break-words text-text">{title}</span>
      {c && (
        <span className="inline-flex items-center gap-1.5 text-sm text-text-2">
          <CategoryDot category={c} />
          {category(c)}
        </span>
      )}
    </p>
  )
}

/** The detail panel of one item (Q9 C): context, journey, technical details, one gold action. */
function DetailPanel({
  item,
  narrow,
  onBack,
  onResolved,
}: {
  item: AttentionItem
  narrow: boolean
  onBack: () => void
  onResolved: (id: string) => void
}) {
  const { t } = useTranslation('admin')
  const title = useItemTitle()(item)
  const category = useCategoryLabel()
  const journeyTitle = useJourneyTitle()
  const ref = useRef<HTMLHeadingElement>(null)
  const request = item.request
  // Opening moves focus to the panel's heading; another item opened replaces it.
  useEffect(() => {
    if (ref.current) focusElement(ref.current)
  }, [item.id])
  const facts: [string, string][] = []
  if (request) {
    facts.push([t('inbox.who.requestedBy'), request.member.displayName])
    facts.push([t('inbox.who.category'), category(request.category)])
    facts.push([t('inbox.who.requested'), formatDateTime(request.createdAt)])
  } else if (item.acquisition) {
    facts.push([t('inbox.who.source'), item.acquisition.serviceName ?? '–'])
    facts.push([t('inbox.who.category'), category(item.acquisition.category)])
  } else if (item.writes) {
    facts.push([t('inbox.who.service'), item.writes.serviceName])
  }
  if (item.since && !request) facts.push([t('inbox.who.since'), formatDateTime(item.since)])
  return (
    <section
      aria-labelledby="inbox-detail-heading"
      className={cn(
        'flex flex-col gap-4 rounded-panel border border-line bg-night-1 p-5 sm:p-6',
        narrow && 'mt-4',
      )}
      onKeyDown={(e) => {
        if (e.key === 'Escape' && narrow) {
          e.stopPropagation()
          onBack()
        }
      }}
    >
      {narrow && (
        <Button variant="ghost" size="sm" className="-ml-2 self-start" onClick={onBack}>
          <ArrowLeftIcon aria-hidden />
          {t('inbox.back')}
        </Button>
      )}
      <div>
        <KindChip kind={item.kind} />
      </div>
      <div className="flex items-start gap-4">
        <ItemThumb item={item} large />
        <h2
          id="inbox-detail-heading"
          ref={ref}
          tabIndex={-1}
          className="type-display min-w-0 text-xl font-bold break-words text-text outline-none"
        >
          {title}
        </h2>
      </div>
      {request?.redacted ? (
        <p className="text-base text-text">{t('inbox.private.lead')}</p>
      ) : (
        <ItemBody item={item} />
      )}
      {facts.length > 0 && (
        <dl className="grid grid-cols-[max-content_1fr] gap-x-5 gap-y-1.5 text-sm">
          {facts.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-text-3">{label}</dt>
              <dd className="text-text">{value}</dd>
            </div>
          ))}
        </dl>
      )}
      {request && !request.redacted && (
        <div className="rounded-card border border-line bg-night-0 p-4">
          <JourneyCard
            title={journeyTitle(request.title, request.kind)}
            units={request.items}
            albums={albumTitles(request.items)}
            admin
          />
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2 border-t border-line pt-4">
        <ItemActions item={item} primary onResolved={onResolved} />
      </div>
    </section>
  )
}
