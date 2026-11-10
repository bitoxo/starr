import i18n from 'i18next'

import { formatDate, formatDayMonth, formatPercent } from '@/lib/format'
import { focusStep, journeyState, type JourneyUnit, type StateView } from '@/lib/states'

/*
 * The second line of the hero's status plaque (spec 0008 A4, amendment 2026-11-07): only what
 * the API says, from the title's journeys (when each step happened) and, for an available
 * title, the media server it opens in.
 */

const STARTED: StateView['key'][] = ['requested', 'awaitingApproval']

/** "Nov 3" this year, "Nov 3, 2025" before (an old date must not read like last week). */
function day(iso: string): string | null {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return null
  return date.getFullYear() === new Date().getFullYear() ? formatDayMonth(iso) : formatDate(iso)
}

/** The first done step of one of `kinds` with a time, and the journey it belongs to. */
function doneStep(units: JourneyUnit[], kinds: string[]) {
  for (const unit of units) {
    const step = unit.journey.steps.find(
      (s) => kinds.includes(s.step) && s.status === 'done' && s.at,
    )
    if (step?.at) return { at: step.at, library: unit.journey.library }
  }
  return null
}

/** "in Jellyfin since Nov 3", "requested Nov 2", "62% downloaded"; null when nothing is known. */
export function plaqueDetail(
  state: StateView,
  units: JourneyUnit[],
  service?: string | null,
): string | null {
  const t = i18n.t
  switch (state.key) {
    case 'available': {
      if (!service) return null
      // "Since" only from a journey that arrived in that very server.
      const ready = doneStep(units, ['ready'])
      const date = ready && ready.library?.includes(service) ? day(ready.at) : null
      return date
        ? t('member:discover.hero.plaque.inServiceSince', { service, date })
        : t('member:discover.hero.plaque.inService', { service })
    }
    case 'partlyAvailable':
      return service ? t('member:discover.hero.plaque.inService', { service }) : null
    case 'downloading':
      return state.percent != null
        ? t('member:discover.hero.plaque.downloaded', { percent: formatPercent(state.percent) })
        : null
    default: {
      if (STARTED.includes(state.key)) {
        const requested = doneStep(units, ['requested', 'awaiting_approval'])
        const date = requested ? day(requested.at) : null
        return date ? t('member:discover.hero.plaque.requested', { date }) : null
      }
      // Someone else's request: their journey is not the member's to show.
      if (state.key === 'requestedByOthers') return null
      // The step it stands at began then (searching, importing, stuck, …).
      const unit = units.find((u) => journeyState(u.journey)?.key === state.key)
      const at = unit ? focusStep(unit.journey)?.at : null
      const date = at ? day(at) : null
      return date ? t('member:discover.hero.plaque.since', { date }) : null
    }
  }
}
