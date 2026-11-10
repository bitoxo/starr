import { ChevronLeftIcon, ChevronRightIcon } from 'lucide-react'
import {
  type FocusEvent,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  useEffect,
  useRef,
  useState,
} from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { z } from 'zod'

import type { DiscoverCategory, DiscoverList, TitleCard } from '@/api/discover'
import { useDiscoverList } from '@/api/discover'
import { CategoryDot } from '@/components/CategoryDot'
import { DatedCard } from '@/components/DatedCard'
import { InfoStrip } from '@/components/InfoStrip'
import { Poster } from '@/components/Poster'
import { PreviewCard } from '@/components/PreviewCard'
import { Skeleton } from '@/components/Skeleton'
import { Button } from '@/components/ui/button'
import { datedWidth, type LaneSource, listSource } from '@/lib/laneSource'
import { isKeyboardFocus } from '@/lib/media'
import { usePreview } from '@/lib/preview'
import { scrollBehavior } from '@/lib/motion'
import { useRovingFocus } from '@/lib/roving'
import { useUrlState } from '@/lib/urlState'
import { useLoadMore } from '@/lib/useLoadMore'
import { cn } from '@/lib/utils'

const TILE = 'w-32 shrink-0 snap-start sm:w-36 lg:w-44'

function Placeholders({ count, square }: { count: number; square: boolean }) {
  return Array.from({ length: count }, (_, i) => (
    <li key={i} aria-hidden className={TILE}>
      <Skeleton className={cn('w-full rounded-card', square ? 'aspect-square' : 'aspect-2/3')} />
    </li>
  ))
}

/** Whether a scroller can scroll back or forward. */
function edgesOf(el: HTMLElement | null) {
  return el
    ? {
        back: el.scrollLeft > 4,
        forward: el.scrollLeft + el.clientWidth < el.scrollWidth - 4,
      }
    : { back: false, forward: false }
}

/**
 * How a lane draws its covers: plain covers, covers with their rank as a large outlined number
 * beside them (`ranked`, spec 0008 Q6 A), or cards with their exact day (`dated`).
 */
export type LaneLook = 'cover' | 'ranked' | 'dated'

/**
 * One list as a horizontal row that loads more while scrolling sideways. The row is a single
 * tab stop (roving tabindex, spec 0008 F4): arrows, Home and End move between the covers, Enter
 * opens one. Covers have no caption (A3): pointer users get the preview card on hover, keyboard
 * users the info strip under the lane while focus is in it, touch users the strip all the time,
 * following the tapped cover (a first tap selects, a second opens; F-5). Ghost previous/next
 * buttons appear on hover and are not in the tab order. The padding keeps the lifted cover's
 * ring inside the scroll box (MEM-14).
 */
export function CoverLane({
  label,
  source,
  square,
  requestable,
  onOpen,
  look = 'cover',
  ranks,
}: {
  /** The lane's name, for its list and its buttons. */
  label: string
  source: LaneSource
  /** Albums: square covers (music, 1:1). */
  square: boolean
  requestable: boolean
  onOpen: (card: TitleCard) => void
  look?: LaneLook
  /** `ranked`: each card's rank, in the order of the cards. */
  ranks?: number[]
}) {
  const { t } = useTranslation(['member', 'common'])
  const scroller = useRef<HTMLUListElement>(null)
  const { cards } = source
  const roving = useRovingFocus<HTMLButtonElement>(cards.length)
  const preview = usePreview()
  // Keyboard focus is in the lane or its strip.
  const [keyboard, setKeyboard] = useState(false)
  // The cover a touch selected (F-5); a tap on it opens it.
  const [tapped, setTapped] = useState<number | null>(null)
  // How the last activation started (`touch` taps select first).
  const pointer = useRef('')
  const stripBox = useRef<HTMLDivElement>(null)
  // Keyboard focus arrived: bring the strip into view with the cover, so its actions are seen.
  // A tap selected a cover: the same, so the strip it filled is not hidden below the tab bar.
  // An open preview card follows the cover through that scroll instead of closing.
  const { keyed } = preview
  useEffect(() => {
    if (keyboard || tapped !== null) {
      keyed()
      stripBox.current?.scrollIntoView?.({ block: 'nearest', behavior: scrollBehavior() })
    }
  }, [keyboard, tapped, keyed])
  // The ghost buttons show on hover: measure then, and while scrolling.
  const [edges, setEdges] = useState(edgesOf(null))
  const update = () => setEdges(edgesOf(scroller.current))
  const page = (direction: 1 | -1) => {
    const el = scroller.current
    el?.scrollBy?.({ left: direction * el.clientWidth * 0.85, behavior: scrollBehavior() })
  }
  // After a failed page, wait for "Try again" instead of asking in a loop.
  const sentinel = useLoadMore<HTMLLIElement>({
    enabled: source.hasNextPage && !source.isFetchingNextPage && !source.isFetchNextPageError,
    onLoad: source.fetchNextPage,
    loaded: source.pages,
    root: scroller,
  })

  const activate = (card: TitleCard, index: number) => {
    preview.close(true)
    const byTouch = pointer.current === 'touch'
    pointer.current = ''
    if (byTouch && tapped !== index) {
      setTapped(index)
      roving.focus(index)
      return
    }
    onOpen(card)
  }

  const onFocus = (event: FocusEvent) => {
    if (isKeyboardFocus(event.target)) setKeyboard(true)
    // Focus on the strip's actions (or anything but a cover) closes the preview card at once,
    // so it never covers the focused action (WCAG 2.4.11).
    if (!(event.target instanceof Element && event.target.closest('[data-preview-tile]'))) {
      preview.close(true)
    }
  }
  const onBlur = (event: FocusEvent<HTMLDivElement>) => {
    const next = event.relatedTarget
    if (!(next instanceof Node) || !event.currentTarget.contains(next)) {
      setKeyboard(false)
      preview.close()
    }
  }

  // "Try again" goes away with its error: focus moves on to the first title the retry brought,
  // never to <body> (F3). Still failing, the button stays where it was.
  const refocus = useRef<{ at: number; fetched: boolean } | null>(null)
  const settled = !source.isFetching && !source.isFetchingNextPage
  const failed = source.isError || source.isFetchNextPageError
  useEffect(() => {
    const pending = refocus.current
    if (pending === null) return
    if (!settled) {
      pending.fetched = true
      return
    }
    if (cards.length > pending.at) {
      refocus.current = null
      roving.focus(pending.at)
    } else if (failed && pending.fetched) {
      // Failed again: the button stayed, and so did focus.
      refocus.current = null
    }
  }, [settled, failed, cards.length, roving])
  const retry = (load: () => void, at: number) => (event: MouseEvent<HTMLButtonElement>) => {
    if (event.currentTarget === document.activeElement) refocus.current = { at, fetched: false }
    load()
  }

  if (source.isError) {
    return (
      <div className="flex flex-wrap items-center gap-3 rounded-card border border-dashed border-line px-5 py-8">
        <p className="text-base text-text-2">{t('discover.listError')}</p>
        <Button size="sm" onClick={retry(source.refetch, 0)}>
          {t('common:actions.retry')}
        </Button>
      </div>
    )
  }
  if (!source.isPending && cards.length === 0) {
    return (
      <p className="rounded-card border border-dashed border-line px-5 py-8 text-base text-text-2">
        {t('discover.listEmpty')}
      </p>
    )
  }
  const current = cards[roving.active]
  // Touch: the strip appears with the first tap (F-5), so a lane shows no gold until then.
  const strip = current && (keyboard || tapped !== null)
  const ghost =
    'absolute top-1/2 z-10 hidden size-10 -translate-y-1/2 cursor-pointer place-items-center rounded-full border border-line bg-night-0/90 text-text opacity-0 transition-opacity duration-(--dur-1) group-hover/lane:opacity-100 hover:bg-night-2 disabled:pointer-events-none disabled:opacity-0 [@media(hover:hover)]:grid'
  // Pointer hover and keyboard focus open the cover's preview card (A3, amendment 2026-11-07).
  const hover = (card: TitleCard) => ({
    'data-preview-tile': true,
    onPointerEnter: (event: PointerEvent<HTMLElement>) => preview.enter(event, card),
    onPointerLeave: preview.leave,
    onFocus: (event: FocusEvent<HTMLElement>) => {
      if (isKeyboardFocus(event.target)) preview.focus(event.currentTarget, card)
    },
  })
  return (
    <div className="flex flex-col gap-1" onFocus={onFocus} onBlur={onBlur}>
      <div className="group/lane relative" onPointerEnter={update}>
        <button
          type="button"
          tabIndex={-1}
          aria-label={t('discover.scrollBack', { lane: label })}
          disabled={!edges.back}
          onClick={() => page(-1)}
          className={cn(ghost, '-left-2 sm:left-0')}
        >
          <ChevronLeftIcon aria-hidden className="size-5" />
        </button>
        <ul
          ref={scroller}
          aria-label={label}
          aria-busy={source.isFetching}
          onKeyDown={(event) => {
            pointer.current = ''
            preview.keyed()
            roving.onKeyDown(event)
          }}
          onPointerDown={(event) => {
            pointer.current = event.pointerType
          }}
          onScroll={update}
          className={cn(
            '-mx-4 flex snap-x scroll-px-4 overflow-x-auto px-4 pt-3 pb-4 [scrollbar-width:thin] sm:-mx-10 sm:scroll-px-10 sm:px-10',
            look === 'ranked' ? 'gap-5' : 'gap-4',
          )}
        >
          {cards.map((card, i) => {
            const key = `${card.kind}-${card.id}`
            const open = (c: TitleCard) => activate(c, i)
            if (look === 'ranked') {
              const rank = ranks?.[i] ?? i + 1
              return (
                <li key={key} data-rank={rank} className="flex shrink-0 snap-start items-end">
                  {/* The rank is in the cover's name; the numeral is decoration. */}
                  <span
                    aria-hidden
                    className="type-display rank-number -mr-2 text-[6.5rem] leading-[0.8] font-extrabold select-none sm:-mr-3 sm:text-[8.5rem] lg:text-[9.5rem]"
                  >
                    {rank}
                  </span>
                  <div className="relative z-10 w-28 sm:w-32 lg:w-40" {...hover(card)}>
                    <Poster
                      card={card}
                      onOpen={open}
                      roving={roving.itemProps(i)}
                      prefix={t('category.rankLabel', { rank })}
                    />
                  </div>
                </li>
              )
            }
            if (look === 'dated') {
              return (
                <li
                  key={key}
                  className={cn('shrink-0 snap-start', datedWidth(card))}
                  {...hover(card)}
                >
                  <DatedCard card={card} onOpen={open} roving={roving.itemProps(i)} />
                </li>
              )
            }
            return (
              <li key={key} className={TILE} {...hover(card)}>
                <Poster card={card} onOpen={open} roving={roving.itemProps(i)} />
              </li>
            )
          })}
          {(source.isPending || source.isFetchingNextPage) && (
            <Placeholders count={6} square={square} />
          )}
          {source.isFetchNextPageError && (
            <li
              className={cn(
                TILE,
                'grid place-items-center rounded-card border border-dashed border-line p-3 text-center',
                square ? 'aspect-square' : 'aspect-2/3',
              )}
            >
              <span className="flex flex-col items-center gap-3">
                <span className="text-sm text-text-2">{t('discover.listError')}</span>
                <Button size="sm" onClick={retry(source.fetchNextPage, cards.length)}>
                  {t('common:actions.retry')}
                </Button>
              </span>
            </li>
          )}
          <li ref={sentinel} aria-hidden className="w-px shrink-0" />
        </ul>
        <button
          type="button"
          tabIndex={-1}
          aria-label={t('discover.scrollForward', { lane: label })}
          disabled={!edges.forward}
          onClick={() => page(1)}
          className={cn(ghost, '-right-2 sm:right-0')}
        >
          <ChevronRightIcon aria-hidden className="size-5" />
        </button>
      </div>
      <div ref={stripBox} className="scroll-mb-6">
        {strip && (
          <InfoStrip
            key={`${current.kind}-${current.id}`}
            card={current}
            requestable={requestable}
            onOpen={() => {
              // From the lane's cover, so closing the detail view returns there.
              roving.focus(roving.active)
              onOpen(current)
            }}
          />
        )}
      </div>
      {preview.anchor && (
        <PreviewCard
          key={preview.anchor.id}
          anchor={preview.anchor}
          cardRef={preview.cardRef}
          closing={preview.closing}
          moved={preview.moved}
          requestable={requestable}
          onOpen={onOpen}
          onLeave={preview.leave}
          onHold={preview.hold}
          onClose={() => preview.close(true)}
          onClosed={preview.closed}
        />
      )}
    </div>
  )
}

/** One catalog list of a category as a lane. */
function Lane({
  category,
  list,
  ...rest
}: {
  category: string
  list: DiscoverList
  label: string
  square: boolean
  requestable: boolean
  onOpen: (card: TitleCard) => void
}) {
  const query = useDiscoverList(category, list)
  return <CoverLane source={listSource(query)} {...rest} />
}

/**
 * The lists of a category as tabs: one tab stop, arrows move and switch (spec 0008 F4). The
 * choice lives in the URL (`?lane-movies=popular`, F6, MEM-12). When the row is wider than its
 * room it scrolls, and a round chevron at the edge that has more says so and scrolls on
 * (MEM-16; pointer and touch, the keys reach every tab anyway).
 */
function ListTabs({
  lists,
  chosen,
  label,
  panel,
  onChoose,
}: {
  lists: DiscoverList[]
  chosen: DiscoverList
  label: string
  /** Id of the lane the tabs switch. */
  panel: string
  onChoose: (list: DiscoverList) => void
}) {
  const { t } = useTranslation('member')
  const row = useRef<HTMLDivElement>(null)
  const tabs = useRef<(HTMLButtonElement | null)[]>([])
  const [edges, setEdges] = useState(edgesOf(null))
  const update = () => setEdges(edgesOf(row.current))
  // Measure once laid out and on every resize: the chevron must show before anyone scrolls.
  useEffect(() => {
    const el = row.current
    const measure = () => setEdges(edgesOf(el))
    measure()
    if (!el || typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(measure)
    observer.observe(el)
    return () => observer.disconnect()
  }, [])
  const nudge = (direction: 1 | -1) => {
    const el = row.current
    el?.scrollBy?.({ left: direction * el.clientWidth * 0.6, behavior: scrollBehavior() })
  }
  const chevron =
    'absolute top-1/2 grid size-8 -translate-y-1/2 cursor-pointer place-items-center rounded-full border border-line bg-night-0 text-text-2 hover:text-text max-lg:size-11'

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
    const from = lists.indexOf(chosen)
    const to: number | undefined = {
      ArrowRight: Math.min(lists.length - 1, from + 1),
      ArrowLeft: Math.max(0, from - 1),
      Home: 0,
      End: lists.length - 1,
    }[event.key]
    if (to === undefined) return
    event.preventDefault()
    const next = lists[to]
    if (next === undefined) return
    onChoose(next)
    tabs.current[to]?.focus()
  }

  return (
    <div className="relative flex min-w-0 max-sm:basis-full">
      <div
        ref={row}
        role="tablist"
        aria-label={label}
        onKeyDown={onKeyDown}
        onScroll={update}
        onPointerEnter={update}
        className="-mx-1 flex min-w-0 scroll-px-12 gap-1 overflow-x-auto px-1 py-1 [scrollbar-width:none]"
      >
        {lists.map((l, i) => (
          <button
            key={l}
            ref={(el) => {
              tabs.current[i] = el
            }}
            type="button"
            role="tab"
            aria-selected={l === chosen}
            aria-controls={panel}
            tabIndex={l === chosen ? 0 : -1}
            onClick={() => onChoose(l)}
            className={cn(
              'h-8 shrink-0 cursor-pointer rounded-full px-3.5 text-sm font-medium whitespace-nowrap text-text-2 transition-colors duration-(--dur-1) hover:text-text max-lg:h-11 max-lg:px-4',
              l === chosen
                ? 'bg-night-2 text-text ring-1 ring-line ring-inset'
                : 'hover:bg-night-1',
            )}
          >
            {t(`discover.lists.${l}`)}
          </button>
        ))}
      </div>
      {edges.back && (
        <button
          type="button"
          tabIndex={-1}
          aria-hidden
          data-scroll-hint="back"
          onClick={() => nudge(-1)}
          className={cn(chevron, 'left-0')}
        >
          <ChevronLeftIcon className="size-4" />
        </button>
      )}
      {edges.forward && (
        <button
          type="button"
          tabIndex={-1}
          aria-hidden
          data-scroll-hint="forward"
          onClick={() => nudge(1)}
          className={cn(chevron, 'right-0')}
        >
          <ChevronRightIcon className="size-4" />
        </button>
      )}
    </div>
  )
}

/** "See all" of a lane: its filterable grid (spec 0008 Q6 B), named after the lane. */
export function SeeAllLink({
  to,
  lane,
  className,
}: {
  to: string
  lane: string
  className?: string
}) {
  const { t } = useTranslation('member')
  return (
    <Link
      to={to}
      aria-label={t('category.seeAllOf', { lane })}
      className={cn(
        'inline-flex h-8 shrink-0 items-center gap-1 rounded-full px-2 text-sm font-medium text-text-2 transition-colors duration-(--dur-1) hover:text-text max-lg:h-11',
        className,
      )}
    >
      {t('category.seeAll')}
      <ChevronRightIcon aria-hidden className="size-4" />
    </Link>
  )
}

/** A category's Discover row: heading with its lists to switch between, then the lane. */
export function Swimlane({
  category,
  onOpen,
}: {
  category: DiscoverCategory
  onOpen: (card: TitleCard) => void
}) {
  const { t } = useTranslation(['member', 'common'])
  const first = category.lists[0] ?? 'trending'
  const [chosen, setList] = useUrlState<DiscoverList>(
    `lane-${category.category}`,
    z.custom<DiscoverList>((v) => category.lists.includes(v as DiscoverList)),
    first,
  )
  // A list can vanish after a refetch (e.g. providers removed): fall back to the first.
  const list = category.lists.includes(chosen) ? chosen : first
  const name = t(`categories.${category.category}`)
  const headingId = `lane-${category.category}`
  const panelId = `lane-${category.category}-panel`

  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
        <h2
          id={headingId}
          className="type-display flex items-center gap-2.5 text-xl font-extrabold text-text sm:text-2xl"
        >
          <CategoryDot category={category.category} className="size-2.5" />
          {name}
        </h2>
        <ListTabs
          lists={category.lists}
          chosen={list}
          label={t('discover.listsLabel', { category: name })}
          panel={panelId}
          onChoose={setList}
        />
        <SeeAllLink
          to={`/discover/${category.category}/${list}`}
          lane={t('discover.laneLabel', { category: name, list: t(`discover.lists.${list}`) })}
          className="ml-auto"
        />
      </div>
      {!category.requestable && (
        <p className="text-sm text-text-3">{t('discover.requestsClosed')}</p>
      )}
      <div id={panelId} role="tabpanel" aria-labelledby={headingId}>
        <Lane
          key={list}
          category={category.category}
          label={t('discover.laneLabel', { category: name, list: t(`discover.lists.${list}`) })}
          list={list}
          square={category.kind === 'album'}
          requestable={category.requestable}
          onOpen={onOpen}
        />
      </div>
    </section>
  )
}
