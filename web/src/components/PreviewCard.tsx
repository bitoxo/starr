import { ChevronDownIcon, PlayIcon } from 'lucide-react'
import {
  type CSSProperties,
  type PointerEvent,
  type RefObject,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import { createPortal } from 'react-dom'
import { useTranslation } from 'react-i18next'

import { coverImage, isMusic, tmdbImage } from '@/api/categories'
import { type TitleCard, useOpenIn } from '@/api/discover'
import { Cover } from '@/components/Poster'
import { QuickRequest } from '@/components/QuickRequest'
import { ProgressBar } from '@/components/RequestStatus'
import { Skeleton } from '@/components/Skeleton'
import { TitleFacts } from '@/components/TitleFacts'
import { Button } from '@/components/ui/button'
import { useReducedMotion } from '@/lib/motion'
import type { PreviewAnchor } from '@/lib/preview'
import { cardState, STATES, stateLabel, type StateTone, type StateView } from '@/lib/states'
import { sampleCoverTint } from '@/lib/tint'
import { cn } from '@/lib/utils'

/*
 * Preview card "Morph" (spec 0008 A3, amendment 2026-11-07; pointer and keyboard focus): the
 * cover itself turns into a landscape card. Its area grows out of the cover's rectangle
 * (clip-path), the poster flies (FLIP) to land as a small poster over the backdrop's lower edge,
 * the backdrop fades into the cover's flat tint (no edge, no divider, no text on the image), and
 * the cover's colour glows softly behind the card. Closing runs the same way back, 0.7× as long.
 * The card overlaps the neighbours and stays 8 px inside the content area. It is `aria-hidden`
 * and nothing in it takes focus: keyboard and touch users get the actions in the info strip.
 * Reduced motion: a 120 ms cross-fade in place, nothing flies or grows.
 */

/** Distance kept from the content area's edges. */
const MARGIN = 8
/** The glow's hard cap (also in `.preview-glow`). */
const GLOW = 0.5
/** Exits run 0.7× as long as entrances (design README A2). */
const EXIT = 0.7

/**
 * The motion tokens of `index.css` (design README A2), read where the animation starts: the
 * card animates through the Web Animations API, which takes numbers, not `var()`.
 */
function motion() {
  const css = getComputedStyle(document.documentElement)
  const ms = (name: string, fallback: number) => {
    const value = Number.parseFloat(css.getPropertyValue(name))
    return Number.isFinite(value) ? value : fallback
  }
  const ease = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback
  return {
    /** Small reveals: the backdrop, each text block (`--dur-2`). */
    reveal: ms('--dur-2', 200),
    /** The morph (`--dur-3`). */
    open: ms('--dur-3', 320),
    /** The glow arrives last and slowest (`--dur-4`). */
    glow: ms('--dur-4', 480),
    /** Reduced motion: the cross-fade (`--dur-1`). */
    fade: ms('--dur-1', 120),
    out: ease('--ease-out', 'cubic-bezier(0.2, 0.8, 0.2, 1)'),
    inOut: ease('--ease-in-out', 'cubic-bezier(0.65, 0, 0.35, 1)'),
  }
}

/** Card geometry: width, backdrop height and the landed poster's size. */
function geometry(square: boolean) {
  const width = square ? 340 : 380
  return {
    width,
    media: square ? 112 : Math.round((width * 9) / 16),
    poster: square ? { w: 84, h: 84 } : { w: 76, h: 114 },
  }
}

/** The cover inside a lane tile (the poster's image box, else the tile). */
function coverOf(el: HTMLElement): HTMLElement {
  return el.querySelector<HTMLElement>('button > span') ?? el
}

/** Whether the Web Animations API is there (not in jsdom). */
function canAnimate(el: Element | null): el is HTMLElement {
  return el instanceof HTMLElement && typeof el.animate === 'function'
}

export function PreviewCard({
  anchor,
  cardRef,
  closing,
  moved,
  requestable,
  onOpen,
  onLeave,
  onHold,
  onClose,
  onClosed,
}: {
  anchor: PreviewAnchor
  cardRef: RefObject<HTMLDivElement | null>
  /** Run the close animation, then call `onClosed`. */
  closing: boolean
  /** Changes when the cover moved under the card (the lane following focus): place it again. */
  moved: number
  requestable: boolean
  onOpen: (card: TitleCard) => void
  onLeave: (event: PointerEvent<HTMLElement>) => void
  onHold: () => void
  /** Close at once (before opening the detail view). */
  onClose: () => void
  onClosed: (id: number) => void
}) {
  const { t } = useTranslation('member')
  const reduced = useReducedMotion()
  const { card, el, keyboard } = anchor
  const square = isMusic(card.kind)
  const geo = geometry(square)
  // From the cover the lane already loaded: its tint (in memory only, design README A1) and its
  // picture, so the flying poster is the same image from the cache.
  const [art] = useState(() => {
    const img = el.querySelector('img')
    return {
      tint: img ? sampleCoverTint(img) : null,
      src: img ? img.currentSrc || img.src : null,
    }
  })
  const surface = useRef<HTMLDivElement>(null)
  const poster = useRef<HTMLDivElement>(null)
  const glow = useRef<HTMLDivElement>(null)
  const media = useRef<HTMLSpanElement>(null)
  // The lane's cover is hidden while its morph is on show (the card's poster stands in).
  const hidden = useRef<HTMLElement | null>(null)
  // The entrance, so a close in mid-flight can run it back from where it is.
  const entrance = useRef<Animation[]>([])

  // Centre the card on the cover inside the content area (8 px from its edges, the first and
  // last covers too) and inside the viewport.
  // Measured layout, written straight to the card (no render in between, so the entrance below
  // starts from the placed card).
  const place = () => {
    const root = cardRef.current
    if (!root) return
    const rect = coverOf(el).getBoundingClientRect()
    const frame = document.getElementById('main')?.getBoundingClientRect()
    const minLeft = (frame?.left ?? 0) + MARGIN
    const maxLeft = (frame?.right ?? window.innerWidth) - geo.width - MARGIN
    const left = Math.max(minLeft, Math.min(maxLeft, rect.left + rect.width / 2 - geo.width / 2))
    const top = Math.max(
      MARGIN,
      Math.min(
        window.innerHeight - root.offsetHeight - MARGIN,
        rect.top + rect.height / 2 - root.offsetHeight / 2,
      ),
    )
    root.style.left = `${left}px`
    root.style.top = `${top}px`
    root.style.visibility = 'visible'
  }

  // The cover moved (the lane or page scrolled to follow keyboard focus): follow it.
  useLayoutEffect(() => {
    if (moved > 0) place()
  }, [moved]) // eslint-disable-line react-hooks/exhaustive-deps

  // The FLIP start: the surface clipped to the cover's rectangle, the poster on the cover. A
  // cover of another shape (a dated 16:9 card) does not fly; its poster fades in in place.
  const flip = () => {
    const rect = coverOf(el).getBoundingClientRect()
    const box = surface.current?.getBoundingClientRect()
    const landed = poster.current?.getBoundingClientRect()
    if (!box || !landed) return null
    const scale = landed.width > 0 ? rect.width / landed.width : 1
    const ratio = (r: { width: number; height: number }) => (r.height > 0 ? r.width / r.height : 0)
    const inset = [
      rect.top - box.top,
      box.right - rect.right,
      box.bottom - rect.bottom,
      rect.left - box.left,
    ]
      .map((v) => `${Math.max(0, v).toFixed(1)}px`)
      .join(' ')
    return {
      clip: `inset(${inset} round 10px)`,
      poster: `translate(${rect.left - landed.left}px, ${rect.top - landed.top}px) scale(${scale})`,
      flies: Math.abs(ratio(rect) - ratio(landed)) < 0.15,
    }
  }

  const showCover = () => {
    hidden.current?.style.removeProperty('opacity')
    hidden.current = null
  }
  // Whatever ends the card (closed, replaced, unmounted) brings the cover back, before the next
  // card of the same tile hides it again.
  useLayoutEffect(() => showCover, [])

  // Place, then the entrance.
  useLayoutEffect(() => {
    place()
    const root = cardRef.current
    if (!canAnimate(root)) return
    const m = motion()
    if (reduced) {
      entrance.current = [
        root.animate([{ opacity: 0 }, { opacity: 1 }], { duration: m.fade, fill: 'backwards' }),
      ]
      return
    }
    const start = flip()
    if (!start || !surface.current || !poster.current) return
    const cover = coverOf(el)
    cover.style.setProperty('opacity', '0')
    hidden.current = cover
    const timing: KeyframeAnimationOptions = { duration: m.open, easing: m.out, fill: 'backwards' }
    entrance.current = [
      surface.current.animate(
        [{ clipPath: start.clip }, { clipPath: 'inset(0px 0px 0px 0px round 16px)' }],
        timing,
      ),
      start.flies
        ? poster.current.animate(
            [
              { transform: start.poster, borderRadius: '10px' },
              { transform: 'none', borderRadius: '6px' },
            ],
            timing,
          )
        : poster.current.animate([{ opacity: 0 }, { opacity: 1 }], timing),
      ...[
        media.current?.querySelector('img')?.animate([{ opacity: 0 }, { opacity: 1 }], {
          duration: m.reveal,
          delay: 60,
          fill: 'backwards',
        }),
        glow.current?.animate([{ opacity: 0 }, { opacity: GLOW }], {
          duration: m.glow,
          delay: 140,
          easing: m.out,
          fill: 'backwards',
        }),
      ].filter((a) => a !== undefined),
      // Text blocks stagger 40 ms each (A2), once the area has mostly grown.
      ...Array.from(root.querySelectorAll('[data-stagger]')).map((node, i) =>
        node.animate(
          [
            { opacity: 0, transform: 'translateY(8px)' },
            { opacity: 1, transform: 'none' },
          ],
          { duration: m.reveal, delay: 150 + i * 40, easing: m.out, fill: 'backwards' },
        ),
      ),
    ]
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  // Exit: the same way back, 0.7× as long. In mid-flight the entrance runs back from where it
  // is; once landed, a new morph goes to wherever the cover is now.
  useEffect(() => {
    if (!closing) return
    const root = cardRef.current
    if (!canAnimate(root)) {
      onClosed(anchor.id)
      return
    }
    const done = () => {
      showCover()
      onClosed(anchor.id)
    }
    const m = motion()
    const flying = entrance.current.some((a) => a.playState === 'running')
    if (flying && !reduced) {
      const [area] = entrance.current
      for (const a of entrance.current) {
        a.updatePlaybackRate(1 / EXIT)
        a.reverse()
      }
      area?.addEventListener('finish', done)
      return () => area?.removeEventListener('finish', done)
    }
    const start = hidden.current ? flip() : null
    if (!start || !surface.current || !poster.current) {
      const fade = root.animate([{ opacity: 1 }, { opacity: 0 }], {
        duration: Math.round(m.fade * EXIT),
        fill: 'forwards',
      })
      fade.onfinish = done
      return () => fade.cancel()
    }
    const timing: KeyframeAnimationOptions = {
      duration: Math.round(m.open * EXIT),
      easing: m.inOut,
      fill: 'forwards',
    }
    const parts = [
      ...Array.from(root.querySelectorAll('[data-stagger]')).map((node) =>
        node.animate([{ opacity: 1 }, { opacity: 0 }], {
          duration: Math.round(m.fade * EXIT),
          fill: 'forwards',
        }),
      ),
      glow.current?.animate([{ opacity: GLOW }, { opacity: 0 }], {
        duration: Math.round(m.reveal * EXIT),
        fill: 'forwards',
      }),
      start.flies
        ? poster.current.animate(
            [{ transform: 'none' }, { transform: start.poster, borderRadius: '10px' }],
            timing,
          )
        : poster.current.animate([{ opacity: 1 }, { opacity: 0 }], timing),
    ]
    const area = surface.current.animate(
      [{ clipPath: 'inset(0px 0px 0px 0px round 16px)' }, { clipPath: start.clip }],
      timing,
    )
    area.onfinish = done
    return () => {
      area.cancel()
      for (const a of parts) a?.cancel()
    }
  }, [closing]) // eslint-disable-line react-hooks/exhaustive-deps

  const openDetails = () => {
    // Focus goes to the cover first, so closing the detail view returns there (spec 0008 F3).
    el.querySelector<HTMLElement>('button')?.focus({ preventScroll: true })
    onClose()
    onOpen(card)
  }

  const backdrop = card.backdrop
    ? tmdbImage(card.backdrop, 'w780')
    : card.poster
      ? square
        ? coverImage(card.poster, '500')
        : tmdbImage(card.poster, 'w342')
      : null

  const style: CSSProperties = {
    width: geo.width,
  }

  return createPortal(
    <div
      ref={cardRef}
      aria-hidden
      data-preview-card
      style={style}
      onPointerEnter={onHold}
      onPointerLeave={onLeave}
      // Nothing in the card takes focus: it is a preview, the info strip holds the actions.
      onMouseDown={(event) => event.preventDefault()}
      className="group/card invisible fixed z-50 text-text"
    >
      {/* The cover's colour, softly behind the card (colour from the title, behind it; A1). */}
      <div ref={glow} className="preview-glow">
        {backdrop && <img src={backdrop} alt="" decoding="async" />}
      </div>
      <div
        ref={surface}
        style={art.tint ? ({ '--tint': art.tint } as CSSProperties) : undefined}
        className="tint-surface relative overflow-hidden rounded-panel shadow-[0_0_0_1px_color-mix(in_oklab,var(--color-text)_7%,transparent),0_26px_60px_color-mix(in_oklab,var(--color-night-0)_70%,transparent)]"
      >
        {/* Backdrop and title are one target: interest in the image means "show me more". */}
        <button
          type="button"
          tabIndex={-1}
          data-details
          onClick={openDetails}
          className="block w-full cursor-pointer text-left"
        >
          <span
            ref={media}
            data-square={square || undefined}
            style={{ height: geo.media }}
            className="preview-media block overflow-hidden"
          >
            {backdrop && <img src={backdrop} alt="" decoding="async" />}
          </span>
          <span
            data-stagger
            className="block px-4 pt-2.5"
            style={{ minHeight: geo.poster.h * 0.42, paddingLeft: geo.poster.w + 30 }}
          >
            <span className="type-display line-clamp-2 block text-xl leading-tight font-bold text-text decoration-text/40 underline-offset-4 group-has-[[data-details]:hover]/card:underline">
              {card.title}
            </span>
            {card.artist && <span className="mt-0.5 block text-sm text-text-2">{card.artist}</span>}
            <TitleFacts card={{ ...card, artist: null }} genres={false} className="mt-1.5" />
          </span>
          {card.genres.length > 0 && (
            <span data-stagger className="mt-1 block px-4 text-sm text-text-2">
              {card.genres.join(', ')}
            </span>
          )}
        </button>
        <div data-stagger className="px-4 pt-1 pb-4">
          <PreviewActions
            card={card}
            requestable={requestable}
            onOpen={openDetails}
            details={t('discover.card.details')}
          />
        </div>
      </div>
      {/* The poster sits outside the clipped surface (it flies), so it joins the same target
          by its own click; like everything in the card it is no tab stop. */}
      <div
        ref={poster}
        data-details
        data-preview-poster
        onClick={openDetails}
        style={{
          top: geo.media - geo.poster.h * 0.58,
          width: geo.poster.w,
          height: geo.poster.h,
        }}
        className={cn(
          'absolute left-4 z-10 origin-top-left cursor-pointer overflow-hidden rounded-control shadow-[0_10px_26px_color-mix(in_oklab,var(--color-night-0)_55%,transparent),0_0_0_1px_color-mix(in_oklab,var(--color-text)_10%,transparent)]',
          // Opened by keyboard: the focus ring travels with the cover (WCAG 2.4.11).
          keyboard && 'ring-2 ring-polaris ring-offset-2 ring-offset-night-1',
        )}
      >
        {art.src ? (
          // The lane's own picture (already loaded), so nothing empty flies.
          <img src={art.src} alt="" className="size-full bg-night-2 object-cover" />
        ) : (
          <Cover
            file={card.poster}
            title={card.title}
            kind={card.kind}
            sizes="84px"
            className="size-full rounded-none"
          />
        )}
      </div>
    </div>,
    document.body,
  )
}

/**
 * The card's action row: an available title opens in its media server (the context's one gold
 * action) under a state line; otherwise Request or the state chip, with a bar while it
 * downloads. Then the round Details button.
 */
function PreviewActions({
  card,
  requestable,
  onOpen,
  details,
}: {
  card: TitleCard
  requestable: boolean
  onOpen: () => void
  details: string
}) {
  const { t } = useTranslation('member')
  const state = cardState(card)
  const ready = state?.key === 'available' || state?.key === 'partlyAvailable'
  const openIn = useOpenIn(card, ready)
  return (
    <>
      {ready && state && <StateLine state={state} service={openIn.link?.name} />}
      <div className="mt-3 flex items-center gap-2">
        {ready ? (
          openIn.link ? (
            <Button asChild variant="primary" size="sm">
              <a href={openIn.link.url} target="_blank" rel="noreferrer" tabIndex={-1}>
                <PlayIcon aria-hidden className="fill-current" />
                {t('title.openIn', { service: openIn.link.name })}
              </a>
            </Button>
          ) : (
            openIn.pending && <Skeleton className="h-8 w-36 rounded-full" />
          )
        ) : (
          <QuickRequest card={card} requestable={requestable} onOpen={onOpen} size="sm" inert />
        )}
        <button
          type="button"
          tabIndex={-1}
          aria-label={details}
          onClick={onOpen}
          className="ml-auto grid size-9 shrink-0 cursor-pointer place-items-center rounded-full border border-text/25 text-text transition-colors duration-(--dur-1) hover:border-text/60"
        >
          <ChevronDownIcon aria-hidden className="size-4" />
        </button>
      </div>
      {state?.key === 'downloading' && state.percent != null && (
        <ProgressBar value={state.percent} className="mt-2.5 w-full [&>span:first-child]:flex-1" />
      )}
    </>
  )
}

/** A state's icon colour (the same tones as its chip). */
const TONE_TEXT: Record<StateTone, string> = {
  ok: 'text-ok',
  info: 'text-info',
  warn: 'text-warn',
  bad: 'text-bad',
  neutral: 'text-text-3',
}

/** "Available in Jellyfin": the state's icon in its tone, its label, the server once known. */
function StateLine({ state, service }: { state: StateView; service?: string }) {
  const { t } = useTranslation('member')
  const { icon: Icon, tone } = STATES[state.key]
  const label = stateLabel(state)
  return (
    <p className="mt-2.5 flex items-center gap-1.5 text-sm text-text">
      <Icon aria-hidden className={cn('size-4 shrink-0', TONE_TEXT[tone])} strokeWidth={2} />
      {service ? t('discover.card.stateIn', { state: label, service }) : label}
    </p>
  )
}
