import { useTranslation } from 'react-i18next'

import type { Availability } from '@/api/discover'
import type { AcquisitionState, DownloadStage, RequestState } from '@/api/requests'
import { StateChip } from '@/components/StateChip'
import { formatPercent } from '@/lib/format'
import {
  acquisitionState,
  cardState,
  type JourneyUnit,
  requestState,
  unitsState,
} from '@/lib/states'
import { cn } from '@/lib/utils'

/*
 * The chips of requests, acquisitions and titles. Each maps what it has onto the state table
 * (spec 0008 F1); with journeys at hand the current step wins over the coarser state, so a
 * chip never contradicts the journey beside it (MEM-01).
 */

/**
 * What a member sees of a title or season: their current step, else what the library has, or
 * "Requested by someone" for others' requests. Nothing for titles nobody asked for.
 */
export function AvailabilityChip({
  status,
  requestedByMe = false,
  units = [],
  className,
}: {
  status: Availability
  requestedByMe?: boolean
  units?: JourneyUnit[]
  className?: string
}) {
  const state = cardState({
    status,
    requestedByMe: requestedByMe || units.length > 0,
    journeys: units,
  })
  return state ? <StateChip state={state} className={className} /> : null
}

/** A request's state; `units` (its items' journeys) name the current step when present. */
export function RequestStateChip({
  state,
  stage,
  arrivingIn,
  units = [],
  admin = false,
  className,
}: {
  state: RequestState
  stage?: DownloadStage | null
  arrivingIn?: string | null
  units?: JourneyUnit[]
  admin?: boolean
  className?: string
}) {
  const view = unitsState(units, { admin }) ?? requestState(state, stage, arrivingIn)
  return <StateChip state={view} className={className} />
}

/** An acquisition's state (admins); `held` (pending at a paused source) wins over the rest. */
export function AcquisitionStateChip({
  state,
  held = false,
  stage,
  source,
  arrivingIn,
  units = [],
}: {
  state: AcquisitionState
  held?: boolean
  stage?: DownloadStage | null
  source?: string | null
  arrivingIn?: string | null
  units?: JourneyUnit[]
}) {
  const view =
    (held ? null : unitsState(units, { admin: true })) ??
    acquisitionState(state, { held, stage, source, arrivingIn })
  return <StateChip state={view} />
}

/** Download progress as a thin bar plus the percentage in mono digits. */
export function ProgressBar({ value, className }: { value: number; className?: string }) {
  const { t } = useTranslation()
  const fraction = Math.min(1, Math.max(0, value))
  const percent = Math.round(fraction * 100)
  return (
    <span className={cn('inline-flex items-center gap-2', className)}>
      <span
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
        aria-label={t('requestProgress', { percent })}
        className="h-1 w-16 overflow-hidden rounded-full bg-night-2"
      >
        <span className="block h-full rounded-full bg-info" style={{ width: `${percent}%` }} />
      </span>
      <span className="type-data text-xs text-text-3">{formatPercent(fraction)}</span>
    </span>
  )
}
