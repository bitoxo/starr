import {
  CheckIcon,
  CircleCheckIcon,
  CircleIcon,
  EyeIcon,
  PlayIcon,
  RotateCwIcon,
  XIcon,
} from 'lucide-react'
import { type ReactNode, useEffect, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { toast } from 'sonner'

import { useAccountPlan, useConfirmAccountPlan, useDecideAccount } from '@/api/accounts'
import type { AttentionItem, AttentionKind, PausedWrites } from '@/api/attention'
import { useApiErrorMessage } from '@/api/errors'
import { useSetPrivateUnlock } from '@/api/privacy'
import {
  type AdminRequest,
  REASON_MAX,
  refOf,
  unitsLabel,
  useApproveRequests,
  useRejectRequests,
  useRetryAcquisition,
} from '@/api/requests'
import { type HeldTitle, useSetWritesPaused } from '@/api/services'
import { RequiredMark } from '@/components/FormField'
import { PosterThumb } from '@/components/PosterThumb'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { ElevationCancelled, useElevation } from '@/lib/elevation'
import { formatRelative } from '@/lib/format'
import { PRIVATE_TOAST_PREFIX } from '@/lib/privateGate'
import { cn } from '@/lib/utils'

import { ConfirmPlanDialog } from '../AccountWrites'
import { requestLabel, useApprovalFeedback } from '../approvals'
import { ResumeWritesDialog } from '../ServicesPage'
import { Actions } from './Actions'
import { KIND, useCategoryLabel, useItemTitle } from './items'
import { LibraryActions } from './LibraryActions'

/*
 * One item of the admin attention model (spec 0008 F9) as the Inbox shows it, in a row
 * (variant A) or in the detail panel (variant C): what it is about in plain words, who and
 * when, and its actions. Rows have no gold action (ADM-16: rows of equal rank); the panel has
 * exactly one. Private items stay neutral until the admin shows them (PRIV-08).
 */

const SETUP_LINK = {
  services: '/admin/media/services',
  categories: '/admin/media/categories',
  two_step: '/admin/system/security',
  members: '/admin/people/invitations',
} as const

const KNOWN_CODES = [
  'transient',
  'throttled',
  'credentials_invalid',
  'rejected',
  'not_found',
  'unknown',
] as const

function codeKey(code: string): (typeof KNOWN_CODES)[number] | 'other' {
  return KNOWN_CODES.find((known) => known === code) ?? 'other'
}

/** A plain chip of the item's kind (not focusable, ADM-38). */
export function KindChip({ kind }: { kind: AttentionKind }) {
  const { t } = useTranslation('admin')
  const { icon, tone } = KIND[kind]
  return (
    <StatusChip tone={tone} icon={icon}>
      {t(`inbox.kind.${kind}`)}
    </StatusChip>
  )
}

/** The small picture at the start of a row: the cover, a lock for a neutral row, or the kind. */
export function ItemThumb({ item, large = false }: { item: AttentionItem; large?: boolean }) {
  const title = useItemTitle()(item)
  const request = item.request
  const acquisition = item.acquisition
  const category = request?.category ?? acquisition?.category
  const poster = request?.poster ?? acquisition?.poster
  if (request || acquisition) {
    return (
      <PosterThumb
        file={poster}
        title={title}
        category={category}
        className={large ? (category === 'adult' ? 'w-44' : 'w-18') : undefined}
      />
    )
  }
  const { icon: Icon } = KIND[item.kind]
  return (
    <span
      aria-hidden
      className="grid size-11 shrink-0 place-items-center rounded-card border border-line bg-night-2 text-text-3"
    >
      <Icon className="size-4.5" strokeWidth={1.75} />
    </span>
  )
}

function heldLabel(title: HeldTitle): string {
  if (title.album) return `${title.title} – ${title.album}`
  return title.season != null
    ? `${title.title} S${String(title.season).padStart(2, '0')}`
    : title.title
}

function Technical({ children }: { children: ReactNode }) {
  const { t } = useTranslation('admin')
  return (
    <details className="mt-2 text-sm text-text-3">
      <summary className="w-max cursor-pointer rounded-control text-text-2 hover:text-text">
        {t('inbox.technicalDetails')}
      </summary>
      <pre className="type-data mt-2 max-h-48 overflow-auto rounded-control border border-line bg-night-0 p-3 text-xs whitespace-pre-wrap">
        {children}
      </pre>
    </details>
  )
}

/** What an item is about, below its title: a plain sentence and the next step. */
export function ItemBody({ item }: { item: AttentionItem }) {
  const { t } = useTranslation(['admin', 'common'])
  const category = useCategoryLabel()
  const { request, acquisition } = item
  switch (item.kind) {
    case 'approval': {
      if (!request) return null
      if (request.redacted)
        return <p className="text-sm text-text-2">{t('inbox.private.hidden')}</p>
      const units = unitsLabel(request.items.filter((i) => i.decision === 'pending_approval'))
      return (
        <p className="text-sm text-text-2">
          {[
            units,
            request.year != null ? String(request.year) : null,
            request.follow ? t('inbox.approval.follows') : null,
          ]
            .filter(Boolean)
            .join(' · ') || category(request.category)}
        </p>
      )
    }
    case 'failed':
    case 'stuck': {
      if (!acquisition) return null
      const source = acquisition.serviceName ?? t('acquisitions.sourceRemoved')
      const failed = item.kind === 'failed'
      const detail = failed
        ? [acquisition.lastError, acquisition.errorCode].filter(Boolean).join('\n')
        : acquisition.sourceMessage
      return (
        <div className="text-sm text-text-2">
          <p>
            {failed
              ? acquisition.serviceName
                ? t('inbox.failed.sentence', { source })
                : t('inbox.failed.sentenceRemoved')
              : t('inbox.stuck.sentence', { source })}{' '}
            {failed ? t('inbox.failed.next', { source }) : t('inbox.stuck.next', { source })}
          </p>
          {detail && !acquisition.redacted && <Technical>{detail}</Technical>}
        </div>
      )
    }
    case 'category_closed': {
      const c = item.category
      if (!c) return null
      const name = category(c.category)
      return (
        <p className="text-sm text-text-2">
          {c.sourceName
            ? t('inbox.closed.incomplete', { source: c.sourceName, category: name })
            : t('inbox.closed.noSource', { category: name })}
        </p>
      )
    }
    case 'account_conflict': {
      const c = item.conflict
      if (!c) return null
      return (
        <p className="text-sm text-text-2">
          {c.kind === 'nameTaken'
            ? t('inbox.conflict.nameTaken', { service: c.serviceName })
            : c.kind === 'missing' && c.since
              ? t('needsYou.accounts.conflict.missingSince', { when: formatRelative(c.since) })
              : t(`needsYou.accounts.conflict.${c.kind}`)}
        </p>
      )
    }
    case 'account_failed': {
      const f = item.accountFailure
      if (!f) return null
      return (
        <div className="text-sm text-text-2">
          <p>{t(`needsYou.accounts.code.${codeKey(f.code)}`)}</p>
          {f.detail && <Technical>{f.detail}</Technical>}
        </div>
      )
    }
    case 'account_deviation': {
      const d = item.deviation
      if (!d) return null
      return (
        <p className="text-sm text-text-2">
          {d.actions.map((a) => t(`accounts.step.${a}`, { username: '' })).join(' · ')}.{' '}
          {t('inbox.deviation.hint')}
        </p>
      )
    }
    case 'account_without_password':
      return <p className="text-sm text-text-2">{t('needsYou.accounts.withoutPasswordHint')}</p>
    case 'library_unmapped': {
      const l = item.library
      if (!l) return null
      return (
        <p className="text-sm text-text-2">
          {t('inbox.library.where', {
            server: l.serviceName,
            content: t(`categories.libraries.content.${l.content}`),
          })}
        </p>
      )
    }
    case 'library_stale':
      return <p className="text-sm text-text-2">{t('inbox.stale.hint')}</p>
    case 'album_unlisted': {
      const a = item.album
      if (!a) return null
      return (
        <div className="text-sm text-text-2">
          <p>
            {t('needsYou.unlisted.where', {
              source: a.source ?? '—',
              libraries: a.libraries ?? '—',
            })}
          </p>
          <details className="mt-2">
            <summary className="w-max cursor-pointer rounded-control text-text-2 hover:text-text">
              {t('inbox.technicalDetails')}
            </summary>
            <p className="mt-2 max-w-prose">{t('needsYou.unlisted.hint')}</p>
          </details>
        </div>
      )
    }
    case 'held':
    case 'writes_paused':
      return item.writes ? <WritesBody writes={item.writes} /> : null
    case 'account_plan_held': {
      const p = item.plan
      if (!p) return null
      return (
        <p className="text-sm text-text-2">
          {p.reason === 'overCap'
            ? p.recent > 0
              ? t('needsYou.accounts.held.overCapRecent', {
                  count: p.destructive,
                  cap: p.cap,
                  recent: p.recent,
                })
              : t('needsYou.accounts.held.overCap', { count: p.destructive, cap: p.cap })
            : t('needsYou.accounts.held.noRecentRun')}
        </p>
      )
    }
    case 'setup': {
      const steps = item.setup ?? []
      const done = steps.filter((s) => s.done).length
      const next = steps.findIndex((s) => !s.done)
      return (
        <div className="text-sm text-text-2">
          <p>{t('inbox.setup.done', { done, total: steps.length })}</p>
          <div aria-hidden className="mt-2 h-1 max-w-72 overflow-hidden rounded-full bg-line">
            <i
              className="block h-full bg-ok"
              style={{ width: `${steps.length ? (done / steps.length) * 100 : 0}%` }}
            />
          </div>
          <ul className="mt-3 grid gap-1.5">
            {steps.map((s, i) => (
              <li
                key={s.step}
                className={cn(
                  'flex items-center gap-2.5',
                  s.done && 'text-text-3 line-through decoration-line',
                  i === next && 'text-text',
                )}
              >
                {s.done ? (
                  <CircleCheckIcon aria-hidden className="size-4 text-ok" />
                ) : (
                  <CircleIcon aria-hidden className="size-4" />
                )}
                <span>
                  {t(`inbox.setup.step.${s.step}`)}
                  <span className="sr-only">
                    {' '}
                    · {s.done ? t('inbox.setup.stepDone') : t('inbox.setup.stepOpen')}
                  </span>
                </span>
              </li>
            ))}
          </ul>
        </div>
      )
    }
  }
}

function WritesBody({ writes }: { writes: PausedWrites }) {
  const { t } = useTranslation('admin')
  const when = writes.pausedAt ? formatRelative(writes.pausedAt) : ''
  const held = writes.held
  const more = held ? held.total - held.preview.length : 0
  return (
    <div className="text-sm text-text-2">
      <p>
        {writes.automatic
          ? t('inbox.writes.pausedAuto', { when })
          : t('inbox.writes.pausedBy', { name: writes.pausedBy ?? '–', when })}{' '}
        {writes.kind === 'source'
          ? held && held.total > 0
            ? t('inbox.writes.heldIntro', { count: held.total })
            : held && held.stopTotal > 0
              ? t('inbox.writes.stopsIntro', { count: held.stopTotal })
              : t('inbox.writes.nothingHeld')
          : t('inbox.writes.accountsWait')}
      </p>
      {held && held.total > 0 && (
        <ul className="mt-2 grid gap-1 text-text">
          {held.preview.map((h, i) => (
            <li key={i} className="flex items-center gap-2 truncate">
              <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-text-3" />
              {heldLabel(h)}
            </li>
          ))}
          {more > 0 && <li className="text-text-3">{t('inbox.writes.more', { count: more })}</li>}
        </ul>
      )}
      {held && held.total > 0 && held.stopTotal > 0 && (
        <p className="mt-1">{t('inbox.writes.stopsIntro', { count: held.stopTotal })}</p>
      )}
      {writes.kind === 'accounts' &&
        (writes.canResume ? (
          <p className="mt-2 flex items-center gap-1.5 text-text">
            <CircleCheckIcon aria-hidden className="size-4 text-ok" />
            {t('inbox.writes.cleared')}
          </p>
        ) : (
          <p className="mt-2">
            {t('inbox.writes.waitsFor', { names: writes.adminsWithoutTotp.join(', ') })}
          </p>
        ))}
    </div>
  )
}

/** Who asked (approvals) or Starr, and since when. */
export function ItemWho({ item }: { item: AttentionItem }) {
  const { t } = useTranslation('admin')
  const member = item.request?.member
  return (
    <div className="text-sm leading-snug text-text-2">
      {member ? (
        <span className="text-text">{member.displayName}</span>
      ) : item.kind !== 'setup' ? (
        <span>{t('inbox.starr')}</span>
      ) : null}
      {item.since && (
        <time dateTime={item.since} className="type-data block text-xs text-text-3">
          {formatRelative(item.since)}
        </time>
      )}
    </div>
  )
}

type Done = (id: string) => void

/**
 * The actions of one item. `primary` (the detail panel) gives the first action the Polaris
 * gold; in rows every action is quiet. Inline steps (a decline reason, "Show to decide", a
 * conflict's choice) open inside the item. `onResolved` runs once the item is done.
 */
export function ItemActions({
  item,
  primary = false,
  onResolved,
  openLink,
}: {
  item: AttentionItem
  primary?: boolean
  onResolved: Done
  /** Rows: the detail panel's link ("Open"). */
  openLink?: ReactNode
}) {
  const main = primary ? 'primary' : 'ghost'
  switch (item.kind) {
    case 'approval':
      return item.request ? (
        <ApprovalActions
          item={item}
          request={item.request}
          main={main}
          onResolved={onResolved}
          openLink={openLink}
        />
      ) : null
    case 'failed':
      return <RetryAction item={item} main={main} onResolved={onResolved} openLink={openLink} />
    case 'stuck':
      return (
        <Actions>
          <LinkButton
            to={`/admin/downloads?open=${item.acquisition?.id ?? ''}`}
            variant={main}
            label="inbox.stuck.openDownload"
          />
          {openLink}
        </Actions>
      )
    case 'category_closed':
      return (
        <Actions>
          <ClosedLink item={item} main={main} />
          {openLink}
        </Actions>
      )
    case 'account_conflict':
      return <ConflictActions item={item} main={main} onResolved={onResolved} openLink={openLink} />
    case 'account_failed':
    case 'account_deviation':
    case 'account_without_password': {
      const memberId =
        item.accountFailure?.memberId ?? item.deviation?.memberId ?? item.withoutPassword?.memberId
      return (
        <Actions>
          <LinkButton
            to={`/admin/people/members/${memberId ?? ''}`}
            variant={main}
            label="inbox.conflict.openMember"
          />
          {openLink}
        </Actions>
      )
    }
    case 'library_unmapped':
      return item.library ? (
        <LibraryActions
          library={item.library}
          primary={primary}
          onResolved={() => onResolved(item.id)}
        >
          {openLink}
        </LibraryActions>
      ) : null
    case 'library_stale':
      return (
        <Actions>
          <LinkButton
            to={`/admin/media/categories/${item.staleLibrary?.category ?? ''}`}
            variant={main}
            label="inbox.stale.remap"
          />
          {openLink}
        </Actions>
      )
    case 'album_unlisted':
      return openLink ? <Actions>{openLink}</Actions> : null
    case 'held':
    case 'writes_paused':
      return item.writes ? (
        <ResumeActions
          item={item}
          writes={item.writes}
          main={main}
          onResolved={onResolved}
          openLink={openLink}
        />
      ) : null
    case 'account_plan_held':
      return (
        <Actions>
          <LinkButton
            to={`/admin/media/accounts?service=${item.plan?.serviceId ?? ''}`}
            variant={main}
            label="inbox.plan.review"
          />
          {openLink}
        </Actions>
      )
    case 'setup': {
      const next = item.setup?.find((s) => !s.done)
      return next ? (
        <Actions>
          <SetupLink step={next.step} main={main} />
          {openLink}
        </Actions>
      ) : null
    }
  }
}

function SetupLink({ step, main }: { step: keyof typeof SETUP_LINK; main: 'primary' | 'ghost' }) {
  const { t } = useTranslation('admin')
  return (
    <Button asChild size="sm" variant={main}>
      <Link
        to={SETUP_LINK[step]}
        aria-label={t('inbox.setup.nextLabel', { step: t(`inbox.setup.step.${step}`) })}
      >
        {t('inbox.setup.next')}
      </Link>
    </Button>
  )
}

function ClosedLink({ item, main }: { item: AttentionItem; main: 'primary' | 'ghost' }) {
  const { t } = useTranslation('admin')
  const category = useCategoryLabel()
  const c = item.category
  if (!c) return null
  return (
    <Button asChild size="sm" variant={main}>
      <Link to={`/admin/media/categories/${c.category}`}>
        {t('inbox.closed.open', { category: category(c.category) })}
      </Link>
    </Button>
  )
}

function LinkButton({
  to,
  variant,
  label,
}: {
  to: string
  variant: 'primary' | 'ghost'
  label:
    | 'inbox.stuck.openDownload'
    | 'inbox.conflict.openMember'
    | 'inbox.stale.remap'
    | 'inbox.plan.review'
}) {
  const { t } = useTranslation('admin')
  return (
    <Button asChild size="sm" variant={variant}>
      <Link to={to}>{t(label)}</Link>
    </Button>
  )
}

/** A failed row action says so at the row (ADM-32), other rows stay usable. */
function RowError({ error }: { error: string | null }) {
  const { t } = useTranslation('admin')
  if (!error) return null
  return (
    <p role="alert" className="basis-full text-sm text-bad">
      {t('inbox.rowError', { message: error })}
    </p>
  )
}

function ApprovalActions({
  item,
  request,
  main,
  onResolved,
  openLink,
}: {
  item: AttentionItem
  request: AdminRequest
  main: 'primary' | 'ghost'
  onResolved: Done
  openLink?: ReactNode
}) {
  const { t } = useTranslation('admin')
  const approve = useApproveRequests()
  const reject = useRejectRequests()
  const unlock = useSetPrivateUnlock()
  const feedback = useApprovalFeedback()
  const errorMessage = useApiErrorMessage()
  const [step, setStep] = useState<'decline' | 'unlock' | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [reason, setReason] = useState('')
  const [missing, setMissing] = useState(false)
  const reasonRef = useRef<HTMLInputElement>(null)
  const firstRef = useRef<HTMLButtonElement>(null)
  const approveRef = useRef<HTMLButtonElement>(null)
  const declineRef = useRef<HTMLButtonElement>(null)
  const showRef = useRef<HTMLButtonElement>(null)
  // Leaving a step re-renders the row's buttons: focus goes back to the one that opened it.
  const refocus = useRef(false)
  // After "Show to decide": focus Approve once the row names the title.
  const wantApprove = useRef(false)
  const reasonId = useId()
  const errorId = `${reasonId}-error`
  const label = request.redacted ? request.title : requestLabel(request)
  const isPrivate = request.category === 'adult'

  // After "Show to decide" the same row names the title: focus goes to its Approve.
  useEffect(() => {
    if (wantApprove.current && !request.redacted) {
      wantApprove.current = false
      approveRef.current?.focus()
    }
  }, [request.redacted])
  useEffect(() => {
    if (step === 'decline') reasonRef.current?.focus()
    if (step === 'unlock') firstRef.current?.focus()
    if (step === null && refocus.current) {
      refocus.current = false
      ;(declineRef.current ?? showRef.current)?.focus()
    }
  }, [step])

  const runApprove = () => {
    setError(null)
    approve.mutate([refOf(request)], {
      onSuccess: (result) => {
        if (result.decided.includes(request.id) || result.skipped.includes(request.id)) {
          onResolved(item.id)
        }
        feedback([request], result)
      },
      onError: (e) => setError(errorMessage(e)),
    })
  }

  const runDecline = () => {
    const trimmed = reason.trim()
    if (!trimmed || [...trimmed].length > REASON_MAX) {
      setMissing(true)
      reasonRef.current?.focus()
      return
    }
    setError(null)
    reject.mutate(
      { requests: [refOf(request)], reason: trimmed },
      {
        onSuccess: () => {
          onResolved(item.id)
          if (isPrivate) {
            toast(t('inbox.declinedPrivate'), {
              id: `${PRIVATE_TOAST_PREFIX}declined:${request.id}`,
            })
          } else toast(t('inbox.declined', { title: label }))
        },
        onError: (e) => setError(errorMessage(e)),
      },
    )
  }

  const cancel = () => {
    refocus.current = true
    wantApprove.current = false
    setStep(null)
    setReason('')
    setMissing(false)
  }

  // A neutral private row: decide only after showing it (PRIV-08), never blind.
  if (request.redacted) {
    return (
      <>
        {step !== 'unlock' && (
          <Actions>
            <Button
              ref={showRef}
              size="sm"
              variant={main}
              aria-label={t('inbox.private.showLabel', { id: request.id })}
              onClick={() => setStep('unlock')}
            >
              <EyeIcon aria-hidden />
              {t('inbox.private.show')}
            </Button>
            {openLink}
          </Actions>
        )}
        {step === 'unlock' && (
          <div
            role="group"
            aria-label={t('inbox.private.group')}
            className="flex basis-full flex-wrap items-center gap-2.5 rounded-card bg-night-2 p-3 text-sm text-text-2"
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.stopPropagation()
                cancel()
              }
            }}
          >
            <EyeIcon aria-hidden className="size-4 shrink-0" />
            <span className="min-w-48 flex-1">{t('inbox.private.confirm')}</span>
            <Button
              ref={firstRef}
              size="sm"
              disabled={unlock.isPending}
              onClick={() => {
                // Set before asking: the refetch that names the title may land before
                // the mutation's own callbacks run.
                wantApprove.current = true
                unlock.mutate(true, {
                  onSuccess: () => setStep(null),
                  onError: (e) => {
                    wantApprove.current = false
                    setError(errorMessage(e))
                  },
                })
              }}
            >
              {t('inbox.private.confirmShow')}
            </Button>
            <Button size="sm" variant="ghost" onClick={cancel}>
              {t('inbox.cancel')}
            </Button>
          </div>
        )}
        <RowError error={error} />
      </>
    )
  }

  return (
    <>
      {step !== 'decline' && (
        <Actions>
          <Button
            ref={approveRef}
            size="sm"
            variant={main}
            disabled={approve.isPending}
            aria-label={t('inbox.approveLabel', { title: label })}
            onClick={runApprove}
          >
            <CheckIcon aria-hidden className={main === 'ghost' ? 'text-ok' : undefined} />
            {t('inbox.approve')}
          </Button>
          <Button
            ref={declineRef}
            size="sm"
            variant="ghost"
            aria-label={t('inbox.declineLabel', { title: label })}
            onClick={() => setStep('decline')}
          >
            <XIcon aria-hidden />
            {t('inbox.decline')}
          </Button>
          {openLink}
        </Actions>
      )}
      {step === 'decline' && (
        <form
          noValidate
          aria-label={t('inbox.declineLabel', { title: label })}
          className="flex basis-full flex-wrap items-center gap-2.5 rounded-card bg-night-2 p-3 text-sm"
          onSubmit={(e) => {
            e.preventDefault()
            runDecline()
          }}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.stopPropagation()
              cancel()
            }
          }}
        >
          <label htmlFor={reasonId} className="text-text-2">
            {t('inbox.declineReason')}
            <RequiredMark />
          </label>
          <input
            ref={reasonRef}
            id={reasonId}
            value={reason}
            maxLength={REASON_MAX}
            required
            aria-invalid={missing || undefined}
            aria-describedby={missing ? errorId : undefined}
            placeholder={t('inbox.declinePlaceholder', { member: request.member.displayName })}
            onChange={(e) => {
              setReason(e.target.value)
              if (e.target.value.trim()) setMissing(false)
            }}
            className="h-9 min-w-0 flex-1 basis-48 rounded-control border border-line bg-night-0 px-3 text-base text-text placeholder:text-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-polaris/40"
          />
          <Button
            type="submit"
            size="sm"
            className="border-bad/60 text-bad hover:border-bad max-md:h-11"
            disabled={reject.isPending}
          >
            {t('inbox.declineSubmit')}
          </Button>
          <Button type="button" size="sm" variant="ghost" className="max-md:h-11" onClick={cancel}>
            {t('inbox.cancel')}
          </Button>
          {missing ? (
            <span id={errorId} className="basis-full text-sm text-bad">
              {t('inbox.declineMissing')}
            </span>
          ) : (
            <span className="basis-full text-xs text-text-3">{t('inbox.declineHint')}</span>
          )}
        </form>
      )}
      <RowError error={error} />
    </>
  )
}

function RetryAction({
  item,
  main,
  onResolved,
  openLink,
}: {
  item: AttentionItem
  main: 'primary' | 'ghost'
  onResolved: Done
  openLink?: ReactNode
}) {
  const { t } = useTranslation('admin')
  const title = useItemTitle()(item)
  const retry = useRetryAcquisition()
  const errorMessage = useApiErrorMessage()
  const [error, setError] = useState<string | null>(null)
  const acquisition = item.acquisition
  if (!acquisition) return null
  const isPrivate = acquisition.category === 'adult'
  return (
    <>
      <Actions>
        <Button
          size="sm"
          variant={main}
          disabled={retry.isPending}
          aria-label={t('inbox.failed.submitLabel', { title })}
          onClick={() => {
            setError(null)
            retry.mutate(acquisition.id, {
              onSuccess: () => {
                onResolved(item.id)
                if (isPrivate) {
                  toast(t('inbox.failed.submittedPrivate'), {
                    id: `${PRIVATE_TOAST_PREFIX}retried:${acquisition.id}`,
                  })
                } else toast(t('inbox.failed.submitted', { title }))
              },
              onError: (e) => setError(errorMessage(e)),
            })
          }}
        >
          <RotateCwIcon aria-hidden />
          {t('inbox.failed.submit')}
        </Button>
        <Button asChild size="sm" variant="ghost">
          <Link to={`/admin/downloads?open=${acquisition.id}`}>
            {t('inbox.failed.openDownload')}
          </Link>
        </Button>
        {openLink}
      </Actions>
      <RowError error={error} />
    </>
  )
}

function ConflictActions({
  item,
  main,
  onResolved,
  openLink,
}: {
  item: AttentionItem
  main: 'primary' | 'ghost'
  onResolved: Done
  openLink?: ReactNode
}) {
  const { t } = useTranslation('admin')
  const decide = useDecideAccount()
  const elevation = useElevation()
  const errorMessage = useApiErrorMessage()
  const [choice, setChoice] = useState<'link' | 'none'>('link')
  const [error, setError] = useState<string | null>(null)
  const name = useId()
  const c = item.conflict
  if (!c) return null
  if (c.kind !== 'nameTaken' || !c.remoteId) {
    return (
      <Actions>
        <LinkButton
          to={`/admin/people/members/${c.memberId}`}
          variant={main}
          label="inbox.conflict.openMember"
        />
        {openLink}
      </Actions>
    )
  }
  const remoteId = c.remoteId
  const apply = async () => {
    setError(null)
    try {
      await elevation.run(() =>
        decide.mutateAsync(
          choice === 'link'
            ? { memberId: c.memberId, serviceId: c.serviceId, decision: 'linked', remoteId }
            : { memberId: c.memberId, serviceId: c.serviceId, decision: 'none' },
        ),
      )
      onResolved(item.id)
      toast(
        choice === 'link'
          ? t('inbox.conflict.linked', {
              username: c.username,
              account: c.remoteName ?? remoteId,
              service: c.serviceName,
            })
          : t('inbox.conflict.noAccount', { username: c.username, service: c.serviceName }),
      )
    } catch (e) {
      if (e instanceof ElevationCancelled) return
      setError(errorMessage(e))
    }
  }
  return (
    <>
      {/* The choice and its Apply share one line, centred against each other. */}
      <div className="flex basis-full flex-wrap items-center gap-x-4 gap-y-3 md:[&>[data-actions]]:ml-auto">
        <fieldset className="min-w-0 flex-1 basis-64">
          <legend className="sr-only">{t('inbox.conflict.how')}</legend>
          <div className="flex flex-col gap-2 text-sm text-text">
            <label className="flex cursor-pointer items-center gap-2.5">
              <input
                type="radio"
                name={name}
                checked={choice === 'link'}
                onChange={() => setChoice('link')}
                className="size-4 accent-polaris"
              />
              <span>
                {t('inbox.conflict.link', { username: c.username })}{' '}
                <span className="type-data text-text-2">{c.remoteName ?? remoteId}</span>
              </span>
            </label>
            <label className="flex cursor-pointer items-center gap-2.5">
              <input
                type="radio"
                name={name}
                checked={choice === 'none'}
                onChange={() => setChoice('none')}
                className="size-4 accent-polaris"
              />
              {t('inbox.conflict.none', { username: c.username, service: c.serviceName })}
            </label>
          </div>
        </fieldset>
        <Actions>
          <Button size="sm" variant={main} disabled={decide.isPending} onClick={() => void apply()}>
            <CheckIcon aria-hidden />
            {t('inbox.conflict.apply')}
          </Button>
          {openLink}
        </Actions>
      </div>
      <RowError error={error} />
      {elevation.dialog}
    </>
  )
}

function ResumeActions({
  item,
  writes,
  main,
  onResolved,
  openLink,
}: {
  item: AttentionItem
  writes: PausedWrites
  main: 'primary' | 'ghost'
  onResolved: Done
  openLink?: ReactNode
}) {
  const { t } = useTranslation('admin')
  const setPaused = useSetWritesPaused()
  const errorMessage = useApiErrorMessage()
  const [confirming, setConfirming] = useState(false)
  const [error, setError] = useState<string | null>(null)

  if (writes.kind === 'accounts') {
    return writes.canResume ? (
      <AccountsResume
        item={item}
        writes={writes}
        main={main}
        onResolved={onResolved}
        openLink={openLink}
      />
    ) : (
      <Actions>
        <Button asChild size="sm" variant={main}>
          <Link to="/admin/people/admins">{t('inbox.writes.openAdmins')}</Link>
        </Button>
        {openLink}
      </Actions>
    )
  }

  const resume = () => {
    setError(null)
    setPaused.mutate(
      { id: writes.serviceId, paused: false },
      {
        onSuccess: () => {
          setConfirming(false)
          onResolved(item.id)
          toast(t('inbox.writes.resumed', { service: writes.serviceName }))
        },
        onError: (e) => {
          setConfirming(false)
          setError(errorMessage(e))
        },
      },
    )
  }
  const held = writes.held
  const sendsSomething = held != null && (held.total > 0 || held.stopTotal > 0)
  return (
    <>
      <Actions>
        <Button
          size="sm"
          variant={main}
          disabled={setPaused.isPending}
          onClick={() => (sendsSomething ? setConfirming(true) : resume())}
        >
          <PlayIcon aria-hidden />
          {t('inbox.writes.resume', { service: writes.serviceName })}
        </Button>
        {openLink}
      </Actions>
      <RowError error={error} />
      <ResumeWritesDialog
        name={writes.serviceName}
        held={confirming && held ? held : null}
        pending={setPaused.isPending}
        onConfirm={resume}
        onCancel={() => setConfirming(false)}
      />
    </>
  )
}

/** Resume a media server's account writes with the plan in view (spec 0005 M8, ADM-06). */
function AccountsResume({
  item,
  writes,
  main,
  onResolved,
  openLink,
}: {
  item: AttentionItem
  writes: PausedWrites
  main: 'primary' | 'ghost'
  onResolved: Done
  openLink?: ReactNode
}) {
  const { t } = useTranslation('admin')
  const [open, setOpen] = useState(false)
  const plan = useAccountPlan(open ? writes.serviceId : null)
  const confirm = useConfirmAccountPlan(writes.serviceId)
  const elevation = useElevation()
  const errorMessage = useApiErrorMessage()
  const [error, setError] = useState<string | null>(null)
  const run = async () => {
    const fingerprint = plan.data?.fingerprint
    if (!fingerprint) return
    try {
      await elevation.run(() => confirm.mutateAsync(fingerprint))
      setOpen(false)
      onResolved(item.id)
      toast(t('inbox.writes.resumedAccounts', { service: writes.serviceName }))
    } catch (e) {
      if (e instanceof ElevationCancelled) return
      setOpen(false)
      setError(errorMessage(e))
    }
  }
  return (
    <>
      <Actions>
        <Button size="sm" variant={main} onClick={() => setOpen(true)}>
          <PlayIcon aria-hidden />
          {t('inbox.writes.resumeAccounts', { service: writes.serviceName })}
        </Button>
        {openLink}
      </Actions>
      {open && plan.isPending && (
        <p role="status" className="basis-full text-sm text-text-3">
          {t('inbox.writes.loadingPlan')}
        </p>
      )}
      <RowError error={error ?? (plan.isError ? errorMessage(plan.error) : null)} />
      {plan.data && (
        <ConfirmPlanDialog
          plan={plan.data}
          serviceName={writes.serviceName}
          confirming={open ? 'resume' : null}
          pending={confirm.isPending}
          onConfirm={() => void run()}
          onCancel={() => setOpen(false)}
        />
      )}
      {elevation.dialog}
    </>
  )
}
