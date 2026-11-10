import { Popover as PopoverPrimitive } from 'radix-ui'
import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import type { components } from '@/api/schema'
import { StateChip } from '@/components/StateChip'
import { formatDayMonth, formatTime } from '@/lib/format'
import {
  focusStep,
  type Journey,
  type JourneyStep,
  journeyState,
  stateLabel,
  stepState,
} from '@/lib/states'
import { cn } from '@/lib/utils'

export type { Journey, JourneyStep }
type OpenInLink = components['schemas']['OpenInLink']

/** One unit's journey: the movie (`season` and `album` absent), one season or one album. */
export type UnitJourney = components['schemas']['UnitJourney']

/**
 * Anything with a journey of one unit: a `UnitJourney`, or a request item (its `album` is the
 * album itself rather than its id).
 */
export type JourneyUnit = {
  season?: number | null
  album?: string | { id: string } | null
  /** Music request items: where this album can be listened to. */
  openIn?: OpenInLink[]
  journey: Journey
}

/** The release-group id of a unit's album, if it is one. */
function albumOf(unit: JourneyUnit): string | null {
  if (unit.album == null) return null
  return typeof unit.album === 'string' ? unit.album : unit.album.id
}

/** Hover delay before the card opens, and grace period to move onto it. */
const OPEN_DELAY = 300
const CLOSE_DELAY = 150

/**
 * A status chip that opens its journey: on hover after a short delay, on keyboard focus,
 * and on tap/click (touch has no hover). Escape or leaving closes it. Without journeys the
 * chip stays a plain chip. The steps come from the server; this only renders them.
 */
export function JourneyChip({
  children,
  title,
  units,
  admin = false,
  openIn = [],
  albums = {},
}: {
  /** The status chip. */
  children: ReactNode
  /** Shown at the top of the card: the title (and kind). */
  title: string
  units: JourneyUnit[]
  admin?: boolean
  openIn?: OpenInLink[]
  /** Music: album titles by release-group id, heading each album's track. */
  albums?: Record<string, string>
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  // Focus moves into the card only when it was opened on purpose (click, tap, Enter).
  const [focusCard, setFocusCard] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  // The pointer that started the current press (`mouse`, `touch`, `pen`).
  const pointer = useRef<string | null>(null)
  const anchor = useRef<HTMLButtonElement>(null)
  const content = useRef<HTMLDivElement>(null)
  const contentId = useId()
  // Escape from inside the card hands focus back to the chip, which must not
  // reopen the card then.
  const returnFocus = useRef(false)
  const returning = useRef(false)

  const clear = () => window.clearTimeout(timer.current)
  const later = (next: boolean, delay: number) => {
    clear()
    timer.current = window.setTimeout(() => setOpen(next), delay)
  }
  useEffect(() => clear, [])

  if (units.length === 0) return <>{children}</>

  return (
    <PopoverPrimitive.Root
      open={open}
      onOpenChange={(next) => {
        clear()
        setOpen(next)
      }}
    >
      <PopoverPrimitive.Anchor asChild>
        <button
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-controls={open ? contentId : undefined}
          // A 44 px hit area on touch layouts without moving the row (touch rule).
          className="relative cursor-pointer rounded-full max-lg:after:absolute max-lg:after:inset-x-0 max-lg:after:-inset-y-2.5"
          onPointerEnter={(e) => {
            if (e.pointerType === 'mouse') later(true, OPEN_DELAY)
          }}
          onPointerLeave={(e) => {
            if (e.pointerType === 'mouse') later(false, CLOSE_DELAY)
          }}
          ref={anchor}
          onPointerDown={(e) => {
            pointer.current = e.pointerType
          }}
          onFocus={(e) => {
            // Keyboard focus opens the card without moving focus into it; a click
            // focuses too, but the click itself decides.
            if (returning.current || !focusVisible(e.currentTarget)) return
            setFocusCard(false)
            setOpen(true)
          }}
          onClick={(e) => {
            // Stay inside whatever the chip sits in (a poster tile, a table row).
            e.stopPropagation()
            clear()
            const via = e.detail === 0 ? 'keyboard' : pointer.current
            pointer.current = null
            if (via === 'mouse') {
              // Keeps a card that hovering opened.
              setFocusCard(false)
              setOpen(true)
            } else if (via === 'keyboard') {
              // Enter or Space: open (also when focus opened it) and move into it, so
              // its links and reasons are reachable.
              setFocusCard(true)
              setOpen(true)
              window.requestAnimationFrame(() => content.current?.focus())
            } else {
              // Taps toggle.
              setFocusCard(true)
              setOpen((was) => !was)
            }
          }}
        >
          {children}
        </button>
      </PopoverPrimitive.Anchor>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          side="top"
          align="center"
          sideOffset={8}
          collisionPadding={16}
          ref={content}
          id={contentId}
          tabIndex={-1}
          aria-label={t('journey.cardLabel', { title })}
          onOpenAutoFocus={(e) => {
            if (!focusCard) e.preventDefault()
          }}
          onEscapeKeyDown={() => {
            returnFocus.current = content.current?.contains(document.activeElement) ?? false
          }}
          onCloseAutoFocus={(e) => {
            e.preventDefault()
            if (!returnFocus.current) return
            returnFocus.current = false
            returning.current = true
            anchor.current?.focus()
            returning.current = false
          }}
          // The chip toggles the card itself; pressing it is not "outside".
          onPointerDownOutside={(e) => {
            if (e.target instanceof Node && anchor.current?.contains(e.target)) e.preventDefault()
          }}
          onFocusOutside={(e) => {
            if (e.target instanceof Node && anchor.current?.contains(e.target)) e.preventDefault()
          }}
          onPointerEnter={(e) => {
            if (e.pointerType === 'mouse') clear()
          }}
          onPointerLeave={(e) => {
            if (e.pointerType === 'mouse') later(false, CLOSE_DELAY)
          }}
          onClick={(e) => e.stopPropagation()}
          className={cn(
            'z-50 w-[min(30rem,calc(100vw-2rem))] rounded-panel border border-line bg-night-2 p-4 text-text outline-hidden',
            'data-[state=open]:animate-in data-[state=open]:fade-in-0 motion-safe:data-[state=open]:zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0',
          )}
        >
          <JourneyCard title={title} units={units} admin={admin} openIn={openIn} albums={albums} />
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  )
}

/** Focus that came from the keyboard (not a click or a tap). */
function focusVisible(element: Element): boolean {
  try {
    return element.matches(':focus-visible')
  } catch {
    // Engines without `:focus-visible`: treat focus as keyboard focus.
    return true
  }
}

/** The card's content: title, one track per unit, and what happens now. */
export function JourneyCard({
  title,
  units,
  admin = false,
  openIn = [],
  albums = {},
  layout = 'row',
  heading = true,
}: {
  title: string
  units: JourneyUnit[]
  admin?: boolean
  openIn?: OpenInLink[]
  albums?: Record<string, string>
  /** `column`: steps top to bottom, for narrow places (the detail view's request card). */
  layout?: 'row' | 'column'
  /** Show `title` on top (the request card has its own heading). */
  heading?: boolean
}) {
  const { t } = useTranslation()
  const byFollow = units.some((u) => u.journey.steps[0]?.note === 'by_follow')
  const single = units.length === 1
  // Albums of a music request each open on their own; one unit opens from the card.
  const cardLinks = openIn.length > 0 ? openIn : single ? (units[0]?.openIn ?? []) : []
  return (
    <div className={cn('flex flex-col gap-4', layout === 'row' && 'max-h-[70vh] overflow-y-auto')}>
      {heading && (
        <p className="type-display text-base leading-tight font-bold text-text">{title}</p>
      )}
      {byFollow && !admin && (
        <p className="-mt-2 text-sm text-text-3">
          {/* Albums come from an artist follow. */}
          {t('journey.byFollow', { context: units.some(albumOf) ? 'music' : undefined })}
        </p>
      )}
      {units.map((unit) => {
        const album = albumOf(unit)
        return (
          <section key={album ?? unit.season ?? 'movie'} className="flex flex-col gap-2.5">
            {unit.season != null && !single && (
              <h3 className="type-data text-xs tracking-wide text-text-3 uppercase">
                {t('journey.season', { number: unit.season })}
              </h3>
            )}
            {album != null && !single && (
              <h3 className="text-sm font-medium text-text-2">
                {albums[album] ?? t('journey.album')}
              </h3>
            )}
            {layout === 'column' ? (
              <TrackColumn journey={unit.journey} admin={admin} />
            ) : (
              <Track journey={unit.journey} admin={admin} />
            )}
            {(single || hasNews(unit.journey)) && (
              <Now journey={unit.journey} admin={admin} music={album != null} />
            )}
            {!single && openIn.length === 0 && isWatchable(unit.journey) && (
              <OpenInLinks links={unit.openIn ?? []} />
            )}
          </section>
        )
      })}
      {units.some((u) => isWatchable(u.journey)) && <OpenInLinks links={cardLinks} />}
    </div>
  )
}

function OpenInLinks({ links }: { links: OpenInLink[] }) {
  const { t } = useTranslation()
  if (links.length === 0) return null
  return (
    <div className="flex flex-wrap gap-2">
      {links.map((link) => (
        <a
          key={link.url}
          href={link.url}
          target="_blank"
          rel="noreferrer"
          className="inline-flex items-center rounded-full bg-polaris px-3.5 py-1.5 text-sm font-semibold text-polaris-ink"
        >
          {t('journey.openIn', { name: link.name })}
        </a>
      ))}
    </div>
  )
}

const LIVE: JourneyStep['status'][] = ['current', 'warning', 'failed', 'ended']

/** A chip naming where a journey stands, for places without a state of their own (a season). */
export function JourneyStatusChip({
  journey,
  admin = false,
}: {
  journey: Journey
  admin?: boolean
}) {
  const state = journeyState(journey, { admin })
  return state ? <StateChip state={state} /> : null
}

function isWatchable(journey: Journey) {
  return journey.steps.some((s) => s.step === 'ready' && s.status !== 'upcoming')
}

/** Seasons show their sentence only when something is going on or wrong. */
function hasNews(journey: Journey) {
  return (
    journey.steps.some((s) => s.status !== 'done' && s.status !== 'upcoming') ||
    isWatchable(journey)
  )
}

function useNames(journey: Journey) {
  const { t } = useTranslation()
  return {
    source: journey.source ?? t('downloadStage.someSource'),
    library: journey.library ?? t('downloadStage.someLibrary'),
  }
}

function useStepLabel(journey: Journey, admin: boolean) {
  const { t } = useTranslation()
  const names = useNames(journey)
  return (s: JourneyStep): string => {
    // Where the journey stands reads exactly like the chip (spec 0008 F1).
    if (LIVE.includes(s.status)) return stateLabel(stepState(s, journey, admin))
    if (s.step === 'getting_it' && s.status === 'done') return t('journey.step.getting_it_done')
    if (admin && (s.step === 'searching' || s.step === 'arriving')) {
      return t(`journey.adminStep.${s.step}`, names)
    }
    return t(`journey.step.${s.step}`)
  }
}

const DOT: Record<JourneyStep['status'], string> = {
  done: 'size-2.5 bg-ok',
  current: 'size-3.5 border-2 border-info bg-info/35',
  warning: 'size-3.5 border-2 border-warn bg-warn/35',
  failed: 'size-3 bg-bad',
  ended: 'size-2.5 bg-text-3',
  upcoming: 'size-2.5 border border-text-3',
}

/** The steps left to right: dot, connector, label and time. */
function Track({ journey, admin }: { journey: Journey; admin: boolean }) {
  const { t } = useTranslation()
  const label = useStepLabel(journey, admin)
  const steps = journey.steps
  return (
    <ol className="flex">
      {steps.map((s, i) => {
        const reached = s.status !== 'upcoming'
        const nextReached = i + 1 < steps.length && steps[i + 1]?.status !== 'upcoming'
        const live = LIVE.includes(s.status)
        return (
          <li
            key={`${s.step}-${i}`}
            aria-current={s.status === 'current' || s.status === 'warning' ? 'step' : undefined}
            className="flex min-w-0 flex-1 flex-col items-center gap-1.5 text-center"
          >
            <span className="relative flex h-3.5 w-full items-center justify-center">
              {i > 0 && (
                <span
                  aria-hidden
                  className={cn(
                    'absolute top-1/2 right-1/2 left-0',
                    reached ? 'h-px bg-ok/60' : 'border-t border-dashed border-text-3/60',
                  )}
                />
              )}
              {i + 1 < steps.length && (
                <span
                  aria-hidden
                  className={cn(
                    'absolute top-1/2 right-0 left-1/2',
                    nextReached ? 'h-px bg-ok/60' : 'border-t border-dashed border-text-3/60',
                  )}
                />
              )}
              <span aria-hidden className={cn('relative z-10 rounded-full', DOT[s.status])} />
            </span>
            <span
              className={cn(
                'px-0.5 text-xs leading-tight',
                live ? 'font-semibold text-text' : reached ? 'text-text-2' : 'text-text-3',
              )}
            >
              {label(s)}
              <span className="sr-only"> ({t(`journey.status.${s.status}`)})</span>
            </span>
            {s.at ? (
              <time dateTime={s.at} className="type-data text-[11px] leading-tight text-text-3">
                {formatDayMonth(s.at)}
                <br />
                {formatTime(s.at)}
              </time>
            ) : (
              live &&
              s.status !== 'failed' &&
              s.status !== 'ended' && (
                <span className="type-data text-[11px] leading-tight text-text-3">
                  {t('journey.now')}
                </span>
              )
            )}
            {s.actor && (
              <span className="text-[11px] leading-tight text-text-3">
                {t('journey.by', { name: s.actor })}
              </span>
            )}
          </li>
        )
      })}
    </ol>
  )
}

/** The steps top to bottom: dot and connector on the left, label and time beside them. */
function TrackColumn({ journey, admin }: { journey: Journey; admin: boolean }) {
  const { t } = useTranslation()
  const label = useStepLabel(journey, admin)
  const steps = journey.steps
  return (
    <ol className="flex flex-col">
      {steps.map((s, i) => {
        const reached = s.status !== 'upcoming'
        const last = i + 1 === steps.length
        const nextReached = !last && steps[i + 1]?.status !== 'upcoming'
        const live = LIVE.includes(s.status)
        return (
          <li
            key={`${s.step}-${i}`}
            aria-current={s.status === 'current' || s.status === 'warning' ? 'step' : undefined}
            className="relative flex gap-3 pb-3 last:pb-0"
          >
            <span aria-hidden className="relative flex w-3.5 shrink-0 justify-center pt-1">
              {!last && (
                <span
                  className={cn(
                    'absolute top-3 -bottom-1 left-1/2 -translate-x-1/2',
                    nextReached ? 'w-px bg-ok/60' : 'border-l border-dashed border-text-3/60',
                  )}
                />
              )}
              <span className={cn('relative z-10 rounded-full', DOT[s.status])} />
            </span>
            <span className="flex min-w-0 flex-1 flex-wrap items-baseline justify-between gap-x-3">
              <span
                className={cn(
                  'text-sm leading-snug',
                  live ? 'font-semibold text-text' : reached ? 'text-text-2' : 'text-text-3',
                )}
              >
                {label(s)}
                <span className="sr-only"> ({t(`journey.status.${s.status}`)})</span>
                {s.actor && (
                  <span className="ml-1.5 text-xs text-text-3">
                    {t('journey.by', { name: s.actor })}
                  </span>
                )}
              </span>
              {s.at ? (
                <time dateTime={s.at} className="type-data text-xs text-text-3">
                  {formatDayMonth(s.at)} {formatTime(s.at)}
                </time>
              ) : (
                live &&
                s.status !== 'failed' &&
                s.status !== 'ended' && (
                  <span className="type-data text-xs text-text-3">{t('journey.now')}</span>
                )
              )}
            </span>
          </li>
        )
      })}
    </ol>
  )
}

/** One sentence on what happens now, plus the step's note, reason or source message. */
function Now({
  journey,
  admin,
  music = false,
}: {
  journey: Journey
  admin: boolean
  /** An album: listened to; ready in the music library, or from the source's files without one. */
  music?: boolean
}) {
  const { t } = useTranslation()
  const names = useNames(journey)
  const step = focusStep(journey)
  if (!step) return null
  const percent = step.progress != null ? Math.round(step.progress * 100) : null
  const ns = admin ? 'adminExplain' : 'explain'
  let sentence: string | null
  // Declined and revoked end red with a reason, not as "something went wrong".
  const refused = step.step === 'declined' || step.step === 'revoked'
  if (step.status === 'failed' && !refused) {
    sentence = t(`journey.${ns}.failed`, names)
  } else if (step.step === 'importing' && step.status === 'warning') {
    sentence = admin ? t('journey.adminExplain.importingStuck', names) : null
  } else if (step.step === 'downloading' && step.status === 'warning' && admin) {
    sentence = t('journey.adminExplain.downloadingWarning', names)
  } else if (step.step === 'downloading') {
    sentence =
      percent != null
        ? t(`journey.${ns}.downloading`, { ...names, percent })
        : t(`journey.${ns}.downloadingUnknown`, names)
  } else if (step.step === 'ready') {
    const ready = step.status === 'current' ? 'ready_partly' : 'ready'
    // Music reads "listen"; with a music library it names it, else the source has it.
    const suffix = music ? (journey.library ? '_music_in' : '_music') : ''
    sentence = t(`journey.${ns}.${ready}${suffix}`, names)
  } else if (step.step === 'held') {
    sentence = admin ? t('journey.adminExplain.held', names) : null
  } else if (step.step === 'getting_it' || step.step === 'requested') {
    sentence = null
  } else {
    sentence = t(`journey.${ns}.${step.step}`, names)
  }
  const note =
    step.note && step.note !== 'by_follow' && step.note !== 'partly_ready' ? step.note : null
  return (
    <div className="flex flex-col gap-1 text-sm text-text-2">
      {sentence && <p>{sentence}</p>}
      {note && <p>{t(`journey.note.${note}`)}</p>}
      {step.detail && (
        <p
          className={cn(
            'border-l-2 pl-2.5 text-text',
            step.status === 'failed'
              ? 'border-bad'
              : step.status === 'warning'
                ? 'border-warn'
                : 'border-line',
          )}
        >
          {refused ? t('journey.reason', { reason: step.detail }) : step.detail}
        </p>
      )}
    </div>
  )
}
