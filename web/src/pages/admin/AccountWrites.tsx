import { CheckIcon, PauseIcon, PlayIcon, ShieldAlertIcon, ZapIcon } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { toast } from 'sonner'

import { type AccountPlan, useConfirmAccountPlan, usePauseAccountWrites } from '@/api/accounts'
import { useApiErrorMessage } from '@/api/errors'
import { useSession } from '@/auth/session'
import { ElevationCancelled, useElevation } from '@/lib/elevation'
import { StatusChip, type StatusTone } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { executableSteps } from '@/lib/accounts'
import { formatDateTime, formatRelative } from '@/lib/format'

import { StepList } from './AccountPlan'

/** How many steps the confirmation names (spec 0005 M8: counts, first five steps). */
const PREVIEW = 5

export type Confirming = 'goLive' | 'resume' | 'approve'

const MODE_TONE: Record<AccountPlan['writes']['mode'], StatusTone> = {
  planOnly: 'neutral',
  live: 'ok',
  paused: 'warn',
}

/**
 * Account writes of one media server (spec 0005 M8, D15): plan only until an
 * admin goes live with the plan in view; pause and resume; approve a plan that
 * waits (destructive cap, one-hour rule). Going live, resuming and approving
 * ask for the admin's password again (M10 2) and need two-factor
 * authentication for every active admin (M10 1): until then the panel says
 * who lacks it and leads the admin to set it up.
 */
export function AccountWritesPanel({
  plan,
  serviceName,
}: {
  plan: AccountPlan
  serviceName: string
}) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const elevation = useElevation()
  const confirm = useConfirmAccountPlan(plan.serviceId)
  const pause = usePauseAccountWrites(plan.serviceId)
  const [confirming, setConfirming] = useState<Confirming | null>(null)
  const { writes } = plan
  const ready = plan.complete && plan.fingerprint != null
  const withoutTotp = plan.adminsWithoutTotp

  const runConfirm = async () => {
    const fingerprint = plan.fingerprint
    if (!fingerprint || !confirming) return
    const kind = confirming
    try {
      await elevation.run(() => confirm.mutateAsync(fingerprint))
      setConfirming(null)
      toast(t(`accounts.writes.done.${kind}`, { name: serviceName }))
    } catch (error) {
      if (error instanceof ElevationCancelled) return
      setConfirming(null)
      toast.error(errorMessage(error))
    }
  }

  const runPause = async () => {
    try {
      await pause.mutateAsync()
      toast(t('accounts.writes.done.pause', { name: serviceName }))
    } catch (error) {
      toast.error(errorMessage(error))
    }
  }

  const action: Confirming | null =
    writes.mode === 'planOnly'
      ? 'goLive'
      : writes.mode === 'paused'
        ? 'resume'
        : writes.next === 'held'
          ? 'approve'
          : null
  const Icon = action === 'goLive' ? ZapIcon : action === 'resume' ? PlayIcon : CheckIcon

  return (
    <div className="flex flex-col gap-3 border-b border-line px-4 py-3 md:flex-row md:items-center md:justify-between">
      <div className="flex min-w-0 flex-col gap-1.5">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-semibold text-text">{t('accounts.writes.title')}</span>
          <StatusChip tone={MODE_TONE[writes.mode]}>
            {t(`accounts.writes.mode.${writes.mode}`)}
          </StatusChip>
          {writes.next === 'held' && (
            <StatusChip tone="warn">{t('accounts.writes.heldChip')}</StatusChip>
          )}
        </div>
        <p className="max-w-2xl text-sm text-text-2">
          <NextLine plan={plan} serviceName={serviceName} />
        </p>
        <WhoLine plan={plan} />
        {action && withoutTotp.length > 0 && <TotpFirst missing={withoutTotp} />}
      </div>
      <div className="flex shrink-0 flex-wrap gap-2">
        {writes.mode === 'live' && (
          <Button variant="secondary" disabled={pause.isPending} onClick={runPause}>
            <PauseIcon aria-hidden />
            {t('accounts.writes.pause')}
          </Button>
        )}
        {action && (
          <Button
            variant="primary"
            disabled={!ready || withoutTotp.length > 0}
            onClick={() => setConfirming(action)}
          >
            <Icon aria-hidden />
            {t(`accounts.writes.action.${action}`)}
          </Button>
        )}
      </div>
      <ConfirmPlanDialog
        plan={plan}
        serviceName={serviceName}
        confirming={confirming}
        pending={confirm.isPending}
        onConfirm={runConfirm}
        onCancel={() => setConfirming(null)}
      />
      {elevation.dialog}
    </div>
  )
}

/** Spec 0005 M10 1: who still lacks two-factor authentication, and the way to set it up. */
function TotpFirst({ missing }: { missing: string[] }) {
  const { t } = useTranslation('admin')
  const me = useSession().me?.username
  const others = missing.filter((name) => name !== me)
  const mine = me != null && missing.includes(me)
  return (
    <div
      role="note"
      className="chip mt-1 flex max-w-2xl items-start gap-2.5 rounded-card px-3 py-2.5 text-sm [--chip:var(--color-warn)]"
    >
      <ShieldAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
      <div className="flex flex-col gap-1.5">
        <span className="font-semibold">{t('accounts.writes.totp.title')}</span>
        <span>
          {mine
            ? t('accounts.writes.totp.you')
            : t('accounts.writes.totp.others', { names: others.join(', ') })}
        </span>
        {mine && others.length > 0 && (
          <span>{t('accounts.writes.totp.others', { names: others.join(', ') })}</span>
        )}
        {mine && (
          <Link
            to="/admin/system/security"
            className="self-start font-semibold underline underline-offset-4"
          >
            {t('accounts.writes.totp.setUp')}
          </Link>
        )}
      </div>
    </div>
  )
}

/** What the next run does, in one sentence. */
function NextLine({ plan, serviceName }: { plan: AccountPlan; serviceName: string }) {
  const { t } = useTranslation('admin')
  const { writes } = plan
  const name = serviceName
  switch (writes.next) {
    case 'planOnly':
      return <>{t('accounts.writes.next.planOnly', { name })}</>
    case 'paused':
      return <>{t('accounts.writes.next.paused', { name })}</>
    case 'incomplete':
      return <>{t('accounts.writes.next.incomplete', { name })}</>
    case 'held':
      return writes.holdReason === 'overCap' ? (
        <>
          {writes.recentDestructive > 0
            ? t('accounts.writes.next.overCapRecent', {
                count: plan.counts.destructive,
                cap: writes.cap,
                recent: writes.recentDestructive,
              })
            : t('accounts.writes.next.overCap', {
                count: plan.counts.destructive,
                cap: writes.cap,
              })}
        </>
      ) : (
        <>{t('accounts.writes.next.noRecentRun', { name })}</>
      )
    case 'execute':
      return executableSteps(plan).length > 0 ? (
        <>{t('accounts.writes.next.execute', { name })}</>
      ) : (
        <>{t('accounts.writes.next.inSync', { name })}</>
      )
  }
}

/** Who switched the writes live or paused them, and the last live run. */
function WhoLine({ plan }: { plan: AccountPlan }) {
  const { t } = useTranslation('admin')
  const { writes } = plan
  const parts: { key: string; text: string; at: string }[] = []
  if (writes.mode === 'paused' && writes.pausedAt) {
    parts.push({
      key: 'paused',
      at: writes.pausedAt,
      // `starr`: paused automatically while an admin had no TOTP (M10 1).
      text:
        writes.pausedBy === 'starr'
          ? t('accounts.writes.pausedAuto', { when: formatRelative(writes.pausedAt) })
          : t('accounts.writes.pausedBy', {
              name: writes.pausedBy ?? '–',
              when: formatRelative(writes.pausedAt),
            }),
    })
  } else if (writes.liveSince) {
    parts.push({
      key: 'live',
      at: writes.liveSince,
      text: t('accounts.writes.liveBy', {
        name: writes.liveBy ?? '–',
        when: formatRelative(writes.liveSince),
      }),
    })
  }
  if (writes.lastLiveRunAt) {
    parts.push({
      key: 'run',
      at: writes.lastLiveRunAt,
      text: t('accounts.writes.lastRun', { when: formatRelative(writes.lastLiveRunAt) }),
    })
  }
  if (parts.length === 0) return null
  return (
    <p className="flex flex-wrap gap-x-3 text-xs text-text-3">
      {parts.map((p) => (
        <span key={p.key} title={formatDateTime(p.at)}>
          {p.text}
        </span>
      ))}
    </p>
  )
}

const COUNTS = ['enable', 'disable', 'libraries', 'delete'] as const

/**
 * Confirmation before writes go out: what the run will execute (counts and
 * the first five steps), like resuming a paused source (spec 0003 O5).
 */
export function ConfirmPlanDialog({
  plan,
  serviceName,
  confirming,
  pending,
  onConfirm,
  onCancel,
}: {
  plan: AccountPlan
  serviceName: string
  confirming: Confirming | null
  pending: boolean
  onConfirm: () => void
  onCancel: () => void
}) {
  const { t } = useTranslation('admin')
  const steps = executableSteps(plan)
  const shown = steps.slice(0, PREVIEW)
  const more = steps.length - shown.length
  const kind = confirming ?? 'goLive'
  return (
    <Dialog open={confirming !== null} onOpenChange={(open) => !open && !pending && onCancel()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {t(`accounts.writes.confirm.${kind}.title`, { name: serviceName })}
          </DialogTitle>
          <DialogDescription>
            {t(`accounts.writes.confirm.${kind}.description`, { name: serviceName })}
          </DialogDescription>
        </DialogHeader>
        <DialogBody>
          <dl className="flex flex-wrap gap-x-5 gap-y-2">
            {COUNTS.map((key) => (
              <div key={key} className="flex items-baseline gap-1.5">
                <dd className="type-data text-lg text-text">{plan.counts[key]}</dd>
                <dt className="text-sm text-text-2">{t(`accounts.plan.counts.${key}`)}</dt>
              </div>
            ))}
          </dl>
          {shown.length === 0 ? (
            <p className="text-sm text-text-2">{t('accounts.writes.confirm.nothing')}</p>
          ) : (
            <ul className="flex flex-col gap-2 rounded-card border border-line bg-night-0 px-4 py-3">
              {shown.map(({ username, step }, i) => (
                <li key={i} className="grid grid-cols-[7rem_1fr] gap-2">
                  <span className="truncate font-semibold text-text">{username}</span>
                  <StepList steps={[step]} />
                </li>
              ))}
              {more > 0 && (
                <li className="text-sm text-text-3">
                  {t('accounts.writes.confirm.more', { count: more })}
                </li>
              )}
            </ul>
          )}
          {plan.counts.create > 0 && (
            <p className="text-sm text-text-3">
              {t('accounts.writes.confirm.creates', { count: plan.counts.create })}
            </p>
          )}
          <p className="text-sm text-text-3">{t('accounts.writes.confirm.never')}</p>
          <p className="text-sm text-text-3">{t('accounts.writes.confirm.passwords')}</p>
        </DialogBody>
        <DialogFooter>
          <Button variant="ghost" disabled={pending} onClick={onCancel}>
            {t('accounts.writes.confirm.cancel')}
          </Button>
          <Button variant="primary" disabled={pending} onClick={onConfirm}>
            {t(`accounts.writes.confirm.${kind}.submit`)}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
