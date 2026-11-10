import i18n from 'i18next'
import {
  ArchiveIcon,
  BanIcon,
  CheckIcon,
  CircleCheckIcon,
  CircleDashedIcon,
  CircleXIcon,
  ClockIcon,
  DownloadIcon,
  HourglassIcon,
  ListOrderedIcon,
  type LucideIcon,
  OctagonXIcon,
  PackageIcon,
  PauseIcon,
  SearchIcon,
  TriangleAlertIcon,
  TvIcon,
  Undo2Icon,
  UsersIcon,
} from 'lucide-react'

import type { Availability } from '@/api/discover'
import type { NotificationKind } from '@/api/notifications'
import type { AcquisitionState, DownloadStage, RequestState } from '@/api/requests'
import type { components } from '@/api/schema'
import { formatNumber, formatPercent } from '@/lib/format'

/*
 * One state vocabulary (spec 0008 F1): this table is the only place that names a request's
 * state. Cover chips, request rows, the journey, admin rows and the notification dots all map
 * what the server says (journey step, request or acquisition state, availability) onto it, so
 * one state never has two labels, tones or icons. The chip shows the **current** journey step.
 */

export type Journey = components['schemas']['JourneyDto']
export type JourneyStep = components['schemas']['JourneyStepDto']

/** Anything with the journey of one unit (movie, season or album). */
export type JourneyUnit = {
  journey: Journey
  /** A season of a series, or an album of a music request (for "1 of 2 seasons"). */
  season?: number | null
  album?: unknown
}

export type StateTone = 'ok' | 'info' | 'warn' | 'bad' | 'neutral'

export type StateKey =
  | 'requested'
  | 'requestedByOthers'
  | 'awaitingApproval'
  | 'approved'
  | 'queued'
  | 'held'
  | 'searching'
  | 'downloading'
  | 'importing'
  | 'stuckAtImport'
  | 'needsAttention'
  | 'arriving'
  | 'available'
  | 'partlyAvailable'
  | 'declined'
  | 'withdrawn'
  | 'revoked'
  | 'failed'
  | 'removed'

export const STATES: Record<StateKey, { tone: StateTone; icon: LucideIcon; outline?: boolean }> = {
  requested: { tone: 'info', icon: ClockIcon },
  requestedByOthers: { tone: 'neutral', icon: UsersIcon },
  awaitingApproval: { tone: 'info', icon: HourglassIcon },
  approved: { tone: 'info', icon: CheckIcon },
  queued: { tone: 'info', icon: ListOrderedIcon },
  held: { tone: 'warn', icon: PauseIcon },
  searching: { tone: 'info', icon: SearchIcon },
  downloading: { tone: 'info', icon: DownloadIcon },
  importing: { tone: 'info', icon: PackageIcon },
  stuckAtImport: { tone: 'warn', icon: TriangleAlertIcon },
  needsAttention: { tone: 'warn', icon: TriangleAlertIcon },
  arriving: { tone: 'info', icon: TvIcon },
  available: { tone: 'ok', icon: CircleCheckIcon },
  partlyAvailable: { tone: 'ok', icon: CircleDashedIcon, outline: true },
  declined: { tone: 'bad', icon: CircleXIcon },
  withdrawn: { tone: 'neutral', icon: Undo2Icon },
  revoked: { tone: 'bad', icon: BanIcon },
  failed: { tone: 'bad', icon: OctagonXIcon },
  removed: { tone: 'neutral', icon: ArchiveIcon },
}

/** A state plus what its label needs. */
export type StateView = {
  key: StateKey
  /** Downloading: fraction 0–1. */
  percent?: number | null
  /** Held: the source whose writes are paused. */
  source?: string | null
  /** Arriving: the media server it shows up in. */
  library?: string | null
  /** Partly available: how many units of how many. */
  have?: number
  total?: number
  unit?: 'seasons' | 'tracks' | 'albums'
}

/** The label of a state, e.g. "Downloading 35%", "Arriving in Jellyfin", "1 of 2 seasons". */
export function stateLabel(view: StateView): string {
  const t = i18n.t
  switch (view.key) {
    case 'downloading':
      return view.percent != null
        ? t('state.downloadingPercent', { percent: formatPercent(view.percent) })
        : t('state.downloading')
    case 'arriving':
      return view.library ? t('state.arriving', { library: view.library }) : t('state.arrivingSoon')
    case 'held':
      return view.source ? t('state.held', { source: view.source }) : t('state.heldWrites')
    case 'partlyAvailable':
      return view.unit && view.have != null && view.total != null
        ? t(`state.partly_${view.unit}`, {
            have: formatNumber(view.have),
            total: formatNumber(view.total),
          })
        : t('state.partlyAvailable')
    default:
      return t(`state.${view.key}`)
  }
}

const LIVE: JourneyStep['status'][] = ['current', 'warning', 'failed', 'ended']

/** The step a journey stands at: the live one, or the last one reached. */
export function focusStep(journey: Journey): JourneyStep | undefined {
  return (
    journey.steps.find((s) => LIVE.includes(s.status)) ??
    [...journey.steps].reverse().find((s) => s.status === 'done')
  )
}

/** The state of one journey, from the step it stands at; null without steps. */
export function journeyState(journey: Journey, { admin = false } = {}): StateView | null {
  const step = focusStep(journey)
  if (!step) return null
  return stepState(step, journey, admin)
}

/** The state a single step names (as if it were the current one). */
export function stepState(step: JourneyStep, journey: Journey, admin = false): StateView {
  const refused = step.step === 'declined' || step.step === 'revoked'
  // Paused writes are the reason it waits, not a fault of their own.
  if (step.step === 'held') {
    return admin ? { key: 'held', source: journey.source } : { key: 'approved' }
  }
  if (step.status === 'warning') {
    return step.step === 'importing' || step.note === 'stuck_at_import'
      ? { key: 'stuckAtImport' }
      : { key: 'needsAttention' }
  }
  if (step.status === 'failed' && !refused) return { key: 'failed' }
  switch (step.step) {
    case 'requested':
      return { key: 'requested' }
    case 'awaiting_approval':
      return { key: 'awaitingApproval' }
    case 'approved':
    case 'approved_automatically':
      return { key: 'approved' }
    case 'searching':
      return { key: 'searching' }
    case 'getting_it':
      return step.status === 'done'
        ? { key: 'arriving', library: journey.library }
        : { key: 'searching' }
    case 'downloading':
      return { key: 'downloading', percent: step.progress }
    case 'importing':
      return { key: 'importing' }
    case 'arriving':
      return { key: 'arriving', library: journey.library }
    case 'ready':
      return { key: step.status === 'current' ? 'partlyAvailable' : 'available' }
    case 'declined':
      return { key: 'declined' }
    case 'withdrawn':
    case 'cancelled':
      return { key: 'withdrawn' }
    case 'revoked':
      return { key: 'revoked' }
    case 'removed':
      return { key: 'removed' }
  }
}

/**
 * In-progress states in journey order, attention first: when several units are under way the
 * one needing someone, else the one furthest behind, names the whole.
 */
const PROGRESS: StateKey[] = [
  'failed',
  'stuckAtImport',
  'needsAttention',
  'held',
  'requested',
  'awaitingApproval',
  'approved',
  'searching',
  'downloading',
  'importing',
  'arriving',
]
const ENDED: StateKey[] = ['declined', 'revoked', 'withdrawn', 'removed']
const READY: StateKey[] = ['available', 'partlyAvailable']

function first(views: StateView[], order: StateKey[]): StateView | undefined {
  return [...views].sort((a, b) => order.indexOf(a.key) - order.indexOf(b.key))[0]
}

/** One state for several units (seasons of a series, albums of a request). */
export function unitsState(units: JourneyUnit[], { admin = false } = {}): StateView | null {
  const views = units.flatMap((u) => journeyState(u.journey, { admin }) ?? [])
  if (views.length === 0) return null
  const live = views.filter((v) => PROGRESS.includes(v.key))
  if (live.length > 0) return first(live, PROGRESS) ?? null
  if (views.every((v) => v.key === 'available')) return { key: 'available' }
  if (views.some((v) => READY.includes(v.key))) {
    // "1 of 2 seasons", "3 of 5 albums": one aggregate over the units.
    const unit = units.some((u) => u.album != null)
      ? 'albums'
      : units.some((u) => u.season != null)
        ? 'seasons'
        : undefined
    const have = views.filter((v) => v.key === 'available').length
    return unit && units.length > 1
      ? { key: 'partlyAvailable', have, total: units.length, unit }
      : { key: 'partlyAvailable' }
  }
  return first(views, ENDED) ?? null
}

/** What a cover or title shows: the member's current step, else what the library has. */
export function cardState(
  card: { status: Availability; requestedByMe?: boolean; journeys: JourneyUnit[] },
  { admin = false } = {},
): StateView | null {
  const fromJourneys = unitsState(card.journeys, { admin })
  const fromStatus = availabilityState(card.status, card.requestedByMe ?? false)
  // A finished or ended journey says less than what the library holds now.
  if (fromJourneys && PROGRESS.includes(fromJourneys.key)) return fromJourneys
  return fromStatus ?? fromJourneys
}

/** What the library has of a title, without a journey; null for new titles. */
export function availabilityState(status: Availability, requestedByMe = false): StateView | null {
  switch (status) {
    case 'new':
      return null
    case 'requested':
      return { key: requestedByMe ? 'requested' : 'requestedByOthers' }
    case 'partially_available':
      return { key: 'partlyAvailable' }
    case 'available':
      return { key: 'available' }
  }
}

const REQUEST: Record<RequestState, StateKey> = {
  pending_approval: 'awaitingApproval',
  approved: 'approved',
  submitted: 'searching',
  in_progress: 'downloading',
  partially_available: 'partlyAvailable',
  available: 'available',
  rejected: 'declined',
  cancelled: 'withdrawn',
  failed: 'failed',
  revoked: 'revoked',
  removed: 'removed',
}

/**
 * What an admin decided about a request, from its state now: everything after an approval
 * (submitted, downloading, available, failed, removed) still reads "Approved" (admin bell).
 */
export function decisionOutcome(state: RequestState): StateKey | null {
  switch (state) {
    case 'pending_approval':
      return null
    case 'rejected':
      return 'declined'
    case 'cancelled':
      return 'withdrawn'
    case 'revoked':
      return 'revoked'
    default:
      return 'approved'
  }
}

/** A request's state where no journey is at hand. */
export function requestState(
  state: RequestState,
  stage?: DownloadStage | null,
  arrivingIn?: string | null,
): StateView {
  if (state === 'in_progress' && stage === 'importing') return { key: 'importing' }
  if (state === 'in_progress' && stage === 'arriving')
    return { key: 'arriving', library: arrivingIn }
  return { key: REQUEST[state] }
}

// Admins only: a pending acquisition has not gone to the source yet ("Queued", slice 12).
const ACQUISITION: Record<AcquisitionState, StateKey> = {
  pending: 'queued',
  submitted: 'searching',
  downloading: 'downloading',
  partially_available: 'partlyAvailable',
  available: 'available',
  failed: 'failed',
  cancelled: 'withdrawn',
}

/** An acquisition's state (admins); `held` wins over pending. */
export function acquisitionState(
  state: AcquisitionState,
  {
    held = false,
    stage,
    source,
    arrivingIn,
  }: {
    held?: boolean
    stage?: DownloadStage | null
    source?: string | null
    arrivingIn?: string | null
  } = {},
): StateView {
  if (held) return { key: 'held', source }
  if (state === 'downloading' && stage === 'importing') return { key: 'importing' }
  if (state === 'downloading' && stage === 'arriving') {
    return { key: 'arriving', library: arrivingIn }
  }
  return { key: ACQUISITION[state] }
}

/** The state a notification reports, for its dot; null for kinds that are not a request state. */
export function notificationState(kind: NotificationKind): StateKey | null {
  switch (kind) {
    case 'request_approved':
      return 'approved'
    case 'request_rejected':
      return 'declined'
    case 'request_revoked':
      return 'revoked'
    case 'request_partially_available':
      return 'partlyAvailable'
    case 'request_available':
      return 'available'
    case 'request_failed':
    case 'acquisition_failed':
      return 'failed'
    case 'import_blocked':
      return 'stuckAtImport'
    default:
      return null
  }
}
