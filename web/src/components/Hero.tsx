import { ChevronLeftIcon, ChevronRightIcon, PauseIcon, PlayIcon } from 'lucide-react'
import { type KeyboardEvent, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { tmdbImage } from '@/api/categories'
import {
  type Highlight,
  type TitleCard,
  useDiscoverCategories,
  useHighlights,
  useTitle,
} from '@/api/discover'
import { type Category, CategoryDot } from '@/components/CategoryDot'
import { QueryError } from '@/components/QueryError'
import { QuickRequest } from '@/components/QuickRequest'
import { Skeleton } from '@/components/Skeleton'
import { StatusPlaque } from '@/components/StatusPlaque'
import { TitleFacts } from '@/components/TitleFacts'
import { Button } from '@/components/ui/button'
import { useCarousel } from '@/lib/carousel'
import { scrollBehavior, useReducedMotion } from '@/lib/motion'
import { plaqueDetail } from '@/lib/plaque'
import { cardState } from '@/lib/states'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { cn } from '@/lib/utils'

/*
 * Hero "Bühne" (spec 0008 A4, A5, Q1 A): the mixed top 10 of trending movies and series, one
 * full-bleed backdrop at a time. At ≥ 1024 px the slides cross-fade in one place and the copy
 * sits on the image's scrim; below, the copy sits under a 16:9 image and the slides are a native
 * scroll-snap row for swiping (Q19 B, no scroll hijacking). The hero pulls itself up under the
 * shell's search row, so the image starts at the top of the page.
 */

/** The hero's height at ≥ 1024 px: 62 vh (max 620 px) below the search row it reaches under. */
const STAGE = 'lg:h-[calc(min(62vh,620px)+96px)] lg:min-h-[540px]'
/**
 * Pulls the hero to the top edge of the content area: the member shell's top padding (`lg:py-14`),
 * search row (`h-11`) and its margin (`lg:mb-10`) in `AppShell.tsx` add up to 140 px.
 */
const BLEED = '-mx-4 -mt-8 sm:-mx-10 lg:-mt-[140px]'
/** Below 1024 px the copy keeps one height on every slide, so nothing below jumps. */
const COPY_MIN = 'max-lg:min-h-[18.5rem]'

/** Details are only for the facts row; the hero revisits its titles every round. */
const FACTS_STALE = 10 * 60_000

/** The hero's shape while it loads: it reserves the stage's room (no layout shift). */
export function HeroSkeleton() {
  return (
    <div aria-hidden data-hero-skeleton className={cn('relative', BLEED)}>
      <Skeleton className={cn('aspect-video w-full rounded-none lg:aspect-auto', STAGE)} />
      <div className={cn('flex flex-col gap-3 px-4 pt-4 sm:px-10 lg:hidden', COPY_MIN)}>
        <Skeleton className="h-4 w-40" />
        <Skeleton className="h-8 w-3/4" />
        <Skeleton className="h-4 w-1/2" />
      </div>
      {/* Below 1024 px the top-10 strip sits under the copy: its row is reserved too. */}
      <div className="mt-4 flex gap-2 px-4 sm:px-10 lg:hidden">
        {[0, 1, 2, 3, 4].map((i) => (
          <Skeleton key={i} className="aspect-2/3 w-11 shrink-0 rounded-control" />
        ))}
      </div>
    </div>
  )
}

/** `onActiveChange` reports the slide on show (for the ambient layer behind the page). */
export function Hero({
  category,
  onOpen,
  onActiveChange,
}: {
  /** One category's top 10 (its category page, Q2 A); absent: movies and series mixed. */
  category?: string
  onOpen: (card: TitleCard) => void
  onActiveChange?: (item: Highlight | null) => void
}) {
  const { t } = useTranslation('member')
  const highlights = useHighlights(category)
  if (highlights.isPending) return <HeroSkeleton />
  if (highlights.isError) {
    return (
      <QueryError
        className="mb-4"
        message={t('discover.hero.error')}
        onRetry={() => void highlights.refetch()}
      />
    )
  }
  if (highlights.data.length === 0) return null
  return <Stage items={highlights.data} onOpen={onOpen} onActiveChange={onActiveChange} />
}

function Stage({
  items,
  onOpen,
  onActiveChange,
}: {
  items: Highlight[]
  onOpen: (card: TitleCard) => void
  onActiveChange?: (item: Highlight | null) => void
}) {
  const { t } = useTranslation('member')
  const reduced = useReducedMotion()
  // A detail view over the page holds the hero, so focus can come back to where it was.
  const held = useTitleSheet().ref !== null
  const carousel = useCarousel(items.length, { reduced, held })
  const { index, go } = carousel
  const active = items[index]
  const categories = useDiscoverCategories().data
  const details = useTitle(active ? { kind: active.kind, id: active.id } : null, {
    staleTime: FACTS_STALE,
  })
  const stage = useRef<HTMLDivElement>(null)
  const tabs = useRef<(HTMLButtonElement | null)[]>([])
  // Where focus goes after a keyboard move: the new thumbnail or the new slide's first action.
  const refocus = useRef<'tab' | 'slide' | null>(null)
  const strip = useRef<HTMLDivElement>(null)
  // The slide on show, for the swipe row's settle timer (which outlives a render).
  const current = useRef(index)
  useEffect(() => {
    current.current = index
  }, [index])
  // The swipe row settles before its position names the slide (no fight with the finger).
  const settle = useRef<ReturnType<typeof setTimeout>>(undefined)
  // The other slides' images load once the page is idle (A4: first slide eager).
  const [idle, setIdle] = useState(false)

  useEffect(() => {
    onActiveChange?.(active ?? null)
  }, [active, onActiveChange])

  useEffect(() => {
    const done = () => setIdle(true)
    if (typeof window.requestIdleCallback === 'function') {
      const id = window.requestIdleCallback(done, { timeout: 3000 })
      return () => window.cancelIdleCallback(id)
    }
    const id = window.setTimeout(done, 1500)
    return () => window.clearTimeout(id)
  }, [])

  // Keep the swipe row (below 1024 px) on the current slide (above, the slides overlap) and
  // the current thumbnail inside its scrolling strip, without scrolling the page.
  useEffect(() => {
    const el = stage.current
    if (el && el.scrollWidth > el.clientWidth + 1) {
      const left = index * el.clientWidth
      if (Math.abs(el.scrollLeft - left) >= 2) el.scrollTo?.({ left, behavior: scrollBehavior() })
    }
    const row = strip.current
    const thumb = tabs.current[index]
    if (row && thumb && row.scrollWidth > row.clientWidth + 1) {
      const start = thumb.offsetLeft - row.offsetLeft
      const end = start + thumb.offsetWidth
      if (start < row.scrollLeft || end > row.scrollLeft + row.clientWidth) {
        row.scrollTo?.({ left: start - 8, behavior: scrollBehavior() })
      }
    }
  }, [index])

  useEffect(() => () => clearTimeout(settle.current), [])

  useEffect(() => {
    const target = refocus.current
    refocus.current = null
    if (target === 'tab') tabs.current[index]?.focus()
    if (target === 'slide') {
      stage.current
        ?.querySelector<HTMLElement>(`[data-slide="${index}"] :is(a[href], button:not([disabled]))`)
        ?.focus()
    }
  }, [index])

  const onScroll = () => {
    clearTimeout(settle.current)
    settle.current = setTimeout(() => {
      const el = stage.current
      if (!el || el.clientWidth === 0) return
      const at = Math.round(el.scrollLeft / el.clientWidth)
      if (at === current.current) return
      // A tapped action of the old slide is about to go inert: keep focus in the carousel.
      if (el.contains(document.activeElement)) refocus.current = 'slide'
      go(at)
    }, 150)
  }

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
    const onTab = event.target instanceof HTMLElement && event.target.getAttribute('role') === 'tab'
    const target: number | undefined = {
      ArrowRight: index + 1,
      ArrowLeft: index - 1,
      Home: onTab ? 0 : undefined,
      End: onTab ? items.length - 1 : undefined,
    }[event.key]
    if (target === undefined) return
    event.preventDefault()
    const inSlide =
      event.target instanceof Element && stage.current?.contains(event.target) === true
    refocus.current = onTab ? 'tab' : inSlide ? 'slide' : null
    go(target)
  }

  const requestable = (item: Highlight) =>
    categories?.find((c) => c.category === item.category)?.requestable ?? false

  const control =
    'grid size-11 shrink-0 cursor-pointer place-items-center rounded-full border border-text/25 bg-night-0/80 text-text transition-colors duration-(--dur-1) hover:border-text/60 lg:size-9'

  return (
    <div
      role="region"
      aria-roledescription="carousel"
      aria-label={t('discover.hero.label')}
      onKeyDown={onKeyDown}
      {...carousel.regionProps}
      className={cn('relative', BLEED)}
    >
      <div
        ref={stage}
        onScroll={onScroll}
        className={cn(
          'flex snap-x snap-mandatory overflow-x-auto [scrollbar-width:none] lg:grid lg:snap-none lg:overflow-hidden',
          STAGE,
        )}
      >
        {items.map((item, k) => (
          <Slide
            key={`${item.kind}-${item.id}`}
            item={item}
            position={k}
            total={items.length}
            active={k === index}
            loadImage={k === 0 || k === index || idle}
            details={k === index ? details.data : undefined}
            requestable={requestable(item)}
            reduced={reduced}
            onOpen={onOpen}
          />
        ))}
      </div>

      {/* Announces the slide after the user moves it; quiet while it rotates by itself. */}
      <p aria-live={carousel.live} aria-atomic className="sr-only">
        {active &&
          t('discover.hero.slide', {
            position: index + 1,
            total: items.length,
            title: active.title,
          })}
      </p>
      <div className="relative z-10 mt-4 flex items-center gap-2 px-4 sm:px-10 lg:absolute lg:inset-x-10 lg:bottom-6 lg:mt-0 lg:items-end lg:px-0">
        <div
          ref={strip}
          role="tablist"
          aria-label={t('discover.hero.choose')}
          className="-my-2 flex min-w-0 flex-1 gap-2 overflow-x-auto px-0.5 py-2 [scrollbar-width:none] lg:flex-none"
        >
          {items.map((item, k) => (
            <button
              key={`${item.kind}-${item.id}`}
              ref={(el) => {
                tabs.current[k] = el
              }}
              type="button"
              role="tab"
              aria-selected={k === index}
              aria-current={k === index ? 'true' : undefined}
              aria-controls={`hero-slide-${k}`}
              tabIndex={k === index ? 0 : -1}
              title={item.title}
              onClick={() => go(k)}
              className={cn(
                'relative aspect-2/3 w-11 shrink-0 cursor-pointer overflow-hidden rounded-control bg-night-2 transition-opacity duration-(--dur-2) lg:w-10',
                k === index ? 'opacity-100' : 'opacity-55 hover:opacity-90',
              )}
            >
              <span className="sr-only">{item.title}</span>
              {item.poster && (
                <img
                  src={tmdbImage(item.poster, 'w154')}
                  alt=""
                  loading="lazy"
                  decoding="async"
                  className="size-full object-cover"
                />
              )}
              {k === index && !reduced && (
                <span
                  key={carousel.cycle}
                  aria-hidden
                  data-hero-progress
                  style={{ animationPlayState: carousel.playing ? 'running' : 'paused' }}
                  onAnimationEnd={carousel.advance}
                  className="hero-progress absolute inset-x-0 bottom-0 h-[3px] bg-text"
                />
              )}
            </button>
          ))}
        </div>
        {reduced && (
          <span className="shrink-0 text-xs text-text-2 lg:ml-auto lg:self-center">
            {t('discover.hero.autoplayOff')}
          </span>
        )}
        <div className="flex shrink-0 items-center gap-2 lg:ml-auto lg:self-center">
          <button
            type="button"
            aria-label={t('discover.hero.previous')}
            onClick={() => go(index - 1)}
            className={cn(control, 'max-lg:hidden')}
          >
            <ChevronLeftIcon aria-hidden className="size-4" />
          </button>
          <button
            type="button"
            aria-label={t('discover.hero.next')}
            onClick={() => go(index + 1)}
            className={cn(control, 'max-lg:hidden')}
          >
            <ChevronRightIcon aria-hidden className="size-4" />
          </button>
          {!reduced && (
            <button
              type="button"
              aria-label={carousel.userPaused ? t('discover.hero.play') : t('discover.hero.pause')}
              onClick={carousel.toggle}
              className={control}
            >
              {carousel.userPaused ? (
                <PlayIcon aria-hidden className="size-4" />
              ) : (
                <PauseIcon aria-hidden className="size-4" />
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}

function Slide({
  item,
  position,
  total,
  active,
  loadImage,
  details,
  requestable,
  reduced,
  onOpen,
}: {
  item: Highlight
  position: number
  total: number
  active: boolean
  loadImage: boolean
  details: ReturnType<typeof useTitle>['data']
  requestable: boolean
  reduced: boolean
  onOpen: (card: TitleCard) => void
}) {
  const { t } = useTranslation('member')
  const detailsButton = useRef<HTMLButtonElement>(null)
  const state = cardState(item)
  // Available or partly available: the hero offers no request then (further seasons are chosen
  // in the detail view), so opening it is the slide's one gold action (A4, amended 2026-11-07).
  const ready = state?.key === 'available' || state?.key === 'partlyAvailable'
  const openIn = ready ? details?.openIn[0] : undefined
  return (
    <div
      id={`hero-slide-${position}`}
      data-slide={position}
      role="group"
      aria-roledescription="slide"
      aria-label={t('discover.hero.slide', { position: position + 1, total, title: item.title })}
      inert={!active}
      className={cn(
        'relative w-full shrink-0 snap-start lg:[grid-area:1/1]',
        'lg:transition-opacity lg:duration-(--dur-hero-fade) lg:ease-in-out motion-reduce:transition-none',
        active ? 'lg:opacity-100' : 'lg:opacity-0',
      )}
    >
      <div className="hero-mask relative aspect-video w-full overflow-hidden bg-night-1 lg:absolute lg:inset-0 lg:aspect-auto">
        {item.backdrop && loadImage && (
          <picture className="contents">
            <source media="(min-width: 1024px)" srcSet={tmdbImage(item.backdrop, 'w1280')} />
            <img
              src={tmdbImage(item.backdrop, 'w780')}
              alt=""
              loading={position === 0 ? 'eager' : 'lazy'}
              fetchPriority={position === 0 ? 'high' : 'auto'}
              decoding="async"
              className={cn(
                'size-full object-cover object-[center_25%]',
                // A slow settle while it shows; none under reduced motion.
                !reduced && 'lg:transition-transform lg:duration-[8s] lg:ease-linear',
                !reduced && (active ? 'lg:scale-100' : 'lg:scale-[1.04]'),
              )}
            />
          </picture>
        )}
        <div aria-hidden className="hero-scrim absolute inset-0 max-lg:hidden" />
      </div>

      <div
        className={cn(
          'relative flex flex-col items-start px-4 pt-4 sm:px-10 lg:absolute lg:bottom-32 lg:left-10 lg:max-w-[min(620px,55%)] lg:p-0',
          COPY_MIN,
        )}
      >
        <p className="flex items-center gap-2 text-sm font-medium text-text">
          <span className="type-display rounded-control bg-text px-1.5 text-base leading-snug font-extrabold text-night-0">
            {t('discover.hero.rankBadge', { rank: item.rank })}
          </span>
          <span>{t('discover.hero.rankText')}</span>
        </p>
        <p className="mt-3 flex items-center gap-2 text-sm text-text-2">
          <CategoryDot category={item.category as Category} />
          {t(`discover.kind.${item.kind}`)}
        </p>
        <h2
          title={item.title}
          className="type-display mt-1.5 line-clamp-2 text-2xl font-extrabold text-balance text-text sm:text-3xl xl:text-4xl"
        >
          {item.title}
        </h2>
        <TitleFacts card={item} details={details} className="mt-3" />
        {item.overview && (
          <p className="mt-3 line-clamp-2 max-w-[52ch] text-base text-text">{item.overview}</p>
        )}
        <div className="mt-5 flex flex-wrap items-center gap-2.5">
          <QuickRequest
            card={item}
            requestable={requestable}
            onOpen={onOpen}
            focusAfter={detailsButton}
            className="h-11 px-5.5"
            renderState={(shown) => (
              <>
                <StatusPlaque
                  state={shown}
                  detail={plaqueDetail(shown, item.journeys, openIn?.name)}
                  // On a phone the buttons wrap under the plaque (prototype 08-C).
                  className="max-sm:basis-full"
                />
                <span aria-hidden className="mr-1 h-7 w-px bg-text/20 max-sm:hidden" />
              </>
            )}
          />
          {openIn && (
            <Button asChild variant="primary" className="h-11 px-5.5">
              <a href={openIn.url} target="_blank" rel="noreferrer">
                <PlayIcon aria-hidden className="fill-current" />
                {t('title.openIn', { service: openIn.name })}
              </a>
            </Button>
          )}
          <Button
            ref={detailsButton}
            variant="secondary"
            className="h-11 px-5.5"
            onClick={() => onOpen(item)}
          >
            {t('discover.card.details')}
          </Button>
        </div>
      </div>
    </div>
  )
}
