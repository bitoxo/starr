import { useTranslation } from 'react-i18next'
import { CopyIcon } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'

import {
  type AccountCell,
  type AccountPlan as Plan,
  type AccountState,
  type PlanStep,
  useAccountPlan,
} from '@/api/accounts'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { planStateTone } from '@/lib/accounts'
import { formatDateTime, formatRelative } from '@/lib/format'

import { AccountWritesPanel } from './AccountWrites'

/**
 * A service's internal account ID (often 32 hex characters): not shown under every name, but on
 * hover of this button and copied with it (ADM-27).
 */
export function CopyId({ id, name }: { id: string; name: string }) {
  const { t } = useTranslation('admin')
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(id)
      toast(t('accounts.copyId.done'))
    } catch {
      toast.error(t('accounts.copyId.failed', { id }))
    }
  }
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="size-7 text-text-3"
            aria-label={t('accounts.copyId.label', { name })}
            onClick={() => void copy()}
          >
            <CopyIcon aria-hidden className="size-3.5" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>
          <span className="type-data text-xs">{id}</span>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}

/** A planned state as a chip, with its explanation as a tooltip. */
export function PlanStateChip({ state }: { state: AccountState }) {
  const { t } = useTranslation('admin')
  return (
    <span title={t(`accounts.planHint.${state}`)}>
      <StatusChip tone={planStateTone(state)}>{t(`accounts.planState.${state}`)}</StatusChip>
    </span>
  )
}

/**
 * How the owned account looks in the server (D4, D13): its name there when it
 * differs from the member's, renamed there since it was linked, or not listed
 * any more.
 */
export function AccountNameNotes({
  cell,
  username,
  className,
}: {
  cell: AccountCell
  username: string
  className: string
}) {
  const { t } = useTranslation('admin')
  const now = cell.account?.username
  const linked = cell.linkedUsername
  const renamed = now != null && linked != null && now.toLowerCase() !== linked.toLowerCase()
  const name = now ?? linked ?? cell.account?.remoteId
  return (
    <>
      {renamed ? (
        <span className={className}>{t('accounts.matrix.renamed', { name: now })}</span>
      ) : (
        name &&
        name.toLowerCase() !== username && (
          <span className={className}>{t('accounts.matrix.named', { name })}</span>
        )
      )}
      {cell.missingSince && (
        <span className={`${className} text-warn`} title={formatDateTime(cell.missingSince)}>
          {t('accounts.matrix.missingSince', { when: formatRelative(cell.missingSince) })}
        </span>
      )}
    </>
  )
}

/** Library names; one no longer listed is named by the start of its ID (ADM-27). */
function libraryNames(libs: PlanStep['add'], unknown: (id: string) => string): string {
  return libs.map((l) => l.name ?? unknown(l.id.slice(0, 8))).join(', ')
}

/** Planned steps with their reasons. */
export function StepList({ steps }: { steps: PlanStep[] }) {
  const { t } = useTranslation('admin')
  if (steps.length === 0) return null
  const unknown = (id: string) => t('accounts.step.unknownLibrary', { id })
  return (
    <ul className="flex flex-col gap-1.5">
      {steps.map((step, i) => (
        <li key={i} className="text-sm">
          <span className="font-medium text-text">
            {step.action === 'create'
              ? t('accounts.step.create', { username: step.username ?? '' })
              : t(`accounts.step.${step.action}`)}
          </span>
          {step.fromAll && <span className="text-text-2"> · {t('accounts.step.fromAll')}</span>}
          {step.add.length > 0 && (
            <span className="text-ok">
              {' '}
              {t('accounts.step.add', { names: libraryNames(step.add, unknown) })}
            </span>
          )}
          {step.remove.length > 0 && (
            <span className="text-bad">
              {' '}
              {t('accounts.step.remove', { names: libraryNames(step.remove, unknown) })}
            </span>
          )}
          <div className="text-xs text-text-3">
            {t(`accounts.reason.${step.reason}`)}
            {step.destructive && <> · {t('accounts.step.destructive')}</>}
            {step.drift && (
              <>
                {' · '}
                <span className="text-info">{t('accounts.step.drift')}</span>
              </>
            )}
          </div>
        </li>
      ))}
    </ul>
  )
}

const COUNTS = [
  'create',
  'enable',
  'disable',
  'libraries',
  'delete',
  'undecided',
  'conflicts',
  'waiting',
] as const

function Counts({ plan }: { plan: Plan }) {
  const { t } = useTranslation('admin')
  return (
    <dl className="flex flex-wrap gap-x-5 gap-y-2">
      {COUNTS.map((key) => (
        <div key={key} className="flex items-baseline gap-1.5">
          <dd className="type-data text-lg text-text">{plan.counts[key]}</dd>
          <dt className="text-sm text-text-2">{t(`accounts.plan.counts.${key}`)}</dt>
        </div>
      ))}
    </dl>
  )
}

/** The stored plan of one media server: counts, then every member with something to show. */
export function AccountPlanPanel({
  serviceId,
  serviceName,
}: {
  serviceId: number
  serviceName: string
}) {
  const { t } = useTranslation('admin')
  const plan = useAccountPlan(serviceId)
  if (plan.isPending) return null
  if (plan.isError || !plan.data) {
    return (
      <p role="alert" className="text-bad">
        {t('accounts.errors.unavailable')}
      </p>
    )
  }
  const data = plan.data
  // Members in sync (managed, nothing to do) or hands off are summarised.
  const shown = data.entries.filter(
    (e) => e.steps.length > 0 || !['managed', 'noAccount', 'notNeeded'].includes(e.state),
  )
  const quiet = data.entries.length - shown.length
  return (
    <section aria-labelledby={`plan-${serviceId}`} className="mb-8">
      <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
        <h2 id={`plan-${serviceId}`} className="type-display text-lg font-semibold text-text">
          {t('accounts.plan.title')}
        </h2>
        {data.computedAt && (
          <time
            className="type-data text-sm text-text-3"
            dateTime={data.computedAt}
            title={formatDateTime(data.computedAt)}
          >
            {t('accounts.plan.computed', { when: formatRelative(data.computedAt) })}
          </time>
        )}
      </div>
      {!data.computedAt ? (
        <p className="text-text-3">{t('accounts.plan.none')}</p>
      ) : (
        <div className="rounded-panel border border-line bg-night-1">
          <AccountWritesPanel plan={data} serviceName={serviceName} />
          <div className="border-b border-line px-4 py-3">
            <Counts plan={data} />
            {!data.complete && (
              <p className="mt-2 text-sm text-warn">{t('accounts.plan.incomplete')}</p>
            )}
          </div>
          {shown.length === 0 ? (
            <p className="px-4 py-3 text-text-3">{t('accounts.plan.nothing')}</p>
          ) : (
            <ul className="divide-y divide-line">
              {shown.map((e) => (
                <li
                  key={e.member.id}
                  className="grid gap-2 px-4 py-3 md:grid-cols-[12rem_10rem_1fr] md:items-start"
                >
                  <Link
                    to={`/admin/people/members/${e.member.id}`}
                    className="font-semibold text-text hover:underline"
                  >
                    {e.member.username}
                  </Link>
                  <div>
                    <PlanStateChip state={e.state} />
                    {e.conflictAccount && (
                      <div className="mt-1 text-xs text-text-3">
                        {t('accounts.matrix.nameTakenBy', {
                          name: e.conflictAccount.username ?? e.conflictAccount.remoteId,
                        })}
                      </div>
                    )}
                  </div>
                  <StepList steps={e.steps} />
                </li>
              ))}
            </ul>
          )}
          {quiet > 0 && shown.length > 0 && (
            <p className="border-t border-line px-4 py-2 text-sm text-text-3">
              {t('accounts.plan.quiet', { count: quiet })}
            </p>
          )}
        </div>
      )}
    </section>
  )
}
