import { useTranslation } from 'react-i18next'

import { formatNumber } from '@/lib/format'
import { STATES, stateLabel, type StateTone, type StateView } from '@/lib/states'
import { cn } from '@/lib/utils'

/*
 * Status plaque (spec 0008 A4, amendment 2026-11-07; prototype 08-C): in the hero the state is
 * not a small chip beside 44 px buttons but a borderless plaque of the same height: an icon
 * circle in the state's tone (while downloading a ring with the percentage) and two lines, the
 * state from the one state table and a detail ("in Jellyfin since Nov 3", "requested Nov 2",
 * "62% downloaded"). Only what the API says: without a detail it is one line.
 */

/** The state's colour as `--c` (the same tones as its chip). */
const TONE: Record<StateTone, string> = {
  ok: '[--c:var(--color-ok)]',
  info: '[--c:var(--color-info)]',
  warn: '[--c:var(--color-warn)]',
  bad: '[--c:var(--color-bad)]',
  neutral: '[--c:var(--color-text-3)]',
}

/** A download's share as a ring, the number inside (the second line says it in words). */
function ProgressRing({ value }: { value: number }) {
  const fraction = Math.min(1, Math.max(0, value))
  const circumference = 2 * Math.PI * 16
  return (
    <span aria-hidden className="relative grid size-9 shrink-0 place-items-center">
      <svg viewBox="0 0 36 36" className="absolute inset-0 size-full -rotate-90">
        <circle
          cx="18"
          cy="18"
          r="16"
          fill="var(--color-night-0)"
          fillOpacity="0.92"
          strokeWidth="3"
          className="stroke-[color-mix(in_oklab,var(--c)_22%,var(--color-night-0))]"
        />
        {fraction > 0 && (
          <circle
            cx="18"
            cy="18"
            r="16"
            fill="none"
            strokeWidth="3"
            strokeLinecap="round"
            strokeDasharray={`${fraction * circumference} ${circumference}`}
            className="stroke-(--c)"
          />
        )}
      </svg>
      <span className="type-data relative text-xs font-semibold text-text">
        {formatNumber(Math.round(fraction * 100))}
      </span>
    </span>
  )
}

export function StatusPlaque({
  state,
  detail,
  className,
}: {
  state: StateView
  detail?: string | null
  className?: string
}) {
  // Re-renders when the language changes; the label comes from the state table.
  useTranslation()
  const { icon: Icon, tone } = STATES[state.key]
  const ring = state.key === 'downloading' && state.percent != null
  return (
    <div
      data-status-plaque
      className={cn(
        'flex h-11 max-w-full min-w-0 items-center gap-2.5 pr-1.5',
        TONE[tone],
        className,
      )}
    >
      {ring ? (
        <ProgressRing value={state.percent ?? 0} />
      ) : (
        <span
          aria-hidden
          className="grid size-9 shrink-0 place-items-center rounded-full bg-[color-mix(in_oklab,var(--c)_22%,var(--color-night-0))] text-(--c) shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--c)_50%,transparent)]"
        >
          <Icon className="size-4.5" strokeWidth={2} />
        </span>
      )}
      <span className="flex min-w-0 flex-col">
        <span className="truncate text-base leading-tight font-semibold text-text">
          {/* While downloading the ring and the detail carry the share. */}
          {stateLabel(ring ? { key: 'downloading' } : state)}
        </span>
        {detail && <span className="truncate text-sm leading-snug text-text-2">{detail}</span>}
      </span>
    </div>
  )
}
