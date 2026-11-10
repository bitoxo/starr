import { PlayIcon, ExternalLinkIcon } from 'lucide-react'
import { Dialog as DialogPrimitive, Tabs as TabsPrimitive } from 'radix-ui'
import { type ReactNode, type Ref, useEffect, useId, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import type { CategoryName } from '@/api/categories'
import { type TitleRef, useMyRequests, useWithdrawRequest } from '@/api/discover'
import type { UnitJourney } from '@/components/StatusJourney'
import { useApiErrorMessage } from '@/api/errors'
import { followCutoff, followOf, useFollow, useFollows, useUnfollow } from '@/api/follows'
import type { components } from '@/api/schema'
import { CategoryDot } from '@/components/CategoryDot'
import { StateChip } from '@/components/StateChip'
import { JourneyCard } from '@/components/StatusJourney'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { withdrawables } from '@/lib/detail'
import { focusElement } from '@/lib/returnFocus'
import { formatDay, formatNumber } from '@/lib/format'
import { serviceName } from '@/lib/services'
import type { StateView } from '@/lib/states'
import { cn } from '@/lib/utils'

/*
 * Building blocks the four detail views share (spec 0008 A7): the header, the tabs, the request
 * card with its inline journey, the one follow switch, fact lists and the selection bar.
 */

type OpenInLink = components['schemas']['OpenInLink']
type Attribution = components['schemas']['Attribution']

/** Category line above the title: dot, category, and anything that belongs to it (networks). */
export function CategoryLine({ category, extra }: { category: CategoryName; extra?: string }) {
  const { t } = useTranslation('member')
  return (
    <p className="flex items-center gap-2 text-sm text-text-2">
      <CategoryDot category={category} />
      <span>
        {t(`categories.${category}`)}
        {extra && <span className="text-text-3"> · {extra}</span>}
      </span>
    </p>
  )
}

/**
 * The header: an optional backdrop (movie, series) fading into the surface with its image
 * scrim, the cover overlapping its lower edge, and the text blocks beside it, which rise one
 * after the other on entrance (A2: 40 ms stagger, none with reduced motion).
 */
export function DetailHeader({
  backdrop,
  cover,
  square = false,
  children,
}: {
  /** Proxied backdrop URL and its `srcSet`, or null for music (no backdrops). */
  backdrop: { src: string; srcSet: string } | null
  cover: ReactNode
  square?: boolean
  children: ReactNode
}) {
  return (
    <header className="relative">
      {backdrop ? (
        <div className="detail-backdrop relative aspect-[21/8] min-h-56 w-full overflow-hidden lg:rounded-t-panel">
          <img
            src={backdrop.src}
            srcSet={backdrop.srcSet}
            sizes="(min-width: 1024px) 1120px, 100vw"
            alt=""
            decoding="async"
            className="absolute inset-0 size-full object-cover"
          />
        </div>
      ) : (
        <div className="h-16 sm:h-20" />
      )}
      <div
        className={cn(
          'relative grid items-end gap-4 px-5 sm:gap-6 sm:px-10',
          square
            ? 'grid-cols-[7rem_minmax(0,1fr)] sm:grid-cols-[11rem_minmax(0,1fr)]'
            : 'grid-cols-[6rem_minmax(0,1fr)] sm:grid-cols-[9.5rem_minmax(0,1fr)]',
          backdrop && '-mt-20 sm:-mt-28',
        )}
      >
        <div className="detail-settle">{cover}</div>
        <div className="detail-stagger flex min-w-0 flex-col gap-2 pb-1">{children}</div>
      </div>
    </header>
  )
}

/** The title itself, as the dialog's accessible name. */
export function DetailTitle({ children }: { children: ReactNode }) {
  return (
    <DialogPrimitive.Title className="type-display text-2xl leading-tight font-extrabold break-words text-text sm:text-3xl lg:text-4xl">
      {children}
    </DialogPrimitive.Title>
  )
}

/** Header facts in one row, numbers in Geist Mono. */
export function Facts({ children }: { children: ReactNode }) {
  return (
    <p className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-sm text-text-2 [&>*]:shrink-0">
      {children}
    </p>
  )
}

/** The certification as a small outlined badge ("Rated 12"). */
export function CertificationBadge({ rating }: { rating: string }) {
  const { t } = useTranslation('member')
  return (
    <span
      title={t('discover.card.certification', { rating })}
      className="type-data inline-flex h-5 items-center rounded-control border-[1.5px] border-current px-1.5 text-xs font-semibold text-text"
    >
      <span className="sr-only">{t('discover.card.certification', { rating })}</span>
      <span aria-hidden>{rating}</span>
    </span>
  )
}

/** The view's body: the tabs, and beside them (above them on narrow screens) the aside. */
export function DetailBody({ aside, children }: { aside: ReactNode; children: ReactNode }) {
  return (
    <div className="grid gap-8 px-5 pt-6 pb-12 sm:px-10 lg:grid-cols-[minmax(0,1fr)_20rem] lg:gap-10 lg:pt-8">
      <div className="min-w-0 max-lg:order-2">{children}</div>
      <aside className="flex min-w-0 flex-col gap-3 max-lg:order-1">{aside}</aside>
    </div>
  )
}

export type DetailTab = { value: string; label: string; content: ReactNode; keepMounted?: boolean }

/**
 * The tabs (`role="tablist"`, arrow keys through Radix). The first tab takes focus once the
 * view is there, unless the user already moved focus inside the dialog (A7).
 */
export function DetailTabs({
  tabs,
  value,
  onValueChange,
}: {
  tabs: DetailTab[]
  value: string
  onValueChange: (value: string) => void
}) {
  const { t } = useTranslation('member')
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const dialog = list.current?.closest('[role="dialog"]')
    const active = document.activeElement
    // On the dialog itself, or lost with the view it was in (a discography tile): the first tab.
    if (dialog && (active === dialog || active === document.body || !dialog.contains(active))) {
      list.current?.querySelector<HTMLElement>('[role="tab"][data-state="active"]')?.focus()
    }
  }, [])
  return (
    <TabsPrimitive.Root value={value} onValueChange={onValueChange}>
      <TabsPrimitive.List
        ref={list}
        aria-label={t('detail.tabsLabel')}
        className="-mx-1 flex gap-1 overflow-x-auto border-b border-line px-1 [scrollbar-width:none] max-lg:pr-14"
      >
        {tabs.map((tab) => (
          <TabsPrimitive.Trigger
            key={tab.value}
            value={tab.value}
            className={cn(
              'relative -mb-px h-11 shrink-0 cursor-pointer rounded-t-control px-3.5 text-base whitespace-nowrap text-text-2 transition-colors duration-(--dur-1) hover:text-text',
              'focus-visible:-outline-offset-2 data-[state=active]:text-text',
              'after:absolute after:inset-x-2.5 after:bottom-0 after:h-0.5 after:rounded-full after:bg-transparent data-[state=active]:after:bg-text',
            )}
          >
            {tab.label}
          </TabsPrimitive.Trigger>
        ))}
      </TabsPrimitive.List>
      {tabs.map((tab) => (
        <TabsPrimitive.Content
          key={tab.value}
          value={tab.value}
          forceMount={tab.keepMounted ? true : undefined}
          className="pt-6 outline-none focus-visible:outline-2 focus-visible:outline-offset-4 data-[state=inactive]:hidden"
        >
          {tab.content}
        </TabsPrimitive.Content>
      ))}
    </TabsPrimitive.Root>
  )
}

/** A section heading inside a tab. */
export function SectionHeading({ id, children }: { id?: string; children: ReactNode }) {
  return (
    <h3 id={id} className="type-display text-xl font-bold text-text">
      {children}
    </h3>
  )
}

/** Label/value rows (the Details tab). Rows without a value are left out. */
export function FactList({ rows }: { rows: [string, ReactNode][] }) {
  const shown = rows.filter(([, value]) => value != null && value !== '' && value !== false)
  return (
    <dl className="grid grid-cols-1 gap-x-6 gap-y-1 text-base sm:grid-cols-[minmax(9rem,auto)_1fr] sm:gap-y-3">
      {shown.map(([label, value]) => (
        <div key={label} className="contents">
          <dt className="text-sm text-text-3 max-sm:mt-3 sm:text-base">{label}</dt>
          <dd className="min-w-0 break-words text-text">{value}</dd>
        </div>
      ))}
    </dl>
  )
}

/** The credit a data source asks for (JustWatch for providers, MusicBrainz for genres). */
export function Credit({
  attributions,
  data,
  children,
}: {
  attributions: Attribution[]
  data: Attribution['data']
  /** The credit's wording; defaults to "Data from <source>". */
  children?: (credit: Attribution) => ReactNode
}) {
  const { t } = useTranslation('member')
  const credit = attributions.find((a) => a.data === data)
  if (!credit) return null
  return (
    <p className="text-sm text-text-3">
      <a
        href={credit.url}
        target="_blank"
        rel="noopener noreferrer"
        className="rounded-control underline underline-offset-4 hover:text-text-2"
      >
        {children ? children(credit) : t('detail.credit', { source: credit.source })}
      </a>
    </p>
  )
}

/** A round monogram for people without a picture (cast, artists; A6). */
export function Monogram({ name, className }: { name: string; className?: string }) {
  const initials = name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase() ?? '')
    .join('')
  return (
    <span
      aria-hidden
      className={cn(
        'type-display grid aspect-square place-items-center rounded-full border border-line bg-night-2 font-bold text-text-2',
        className,
      )}
    >
      {initials}
    </span>
  )
}

/**
 * The request card (A7): where the title stands as one chip, a sentence, the one Polaris action
 * (Request, Open in …), the journey inline (MEM-08), "Withdraw request" while allowed (MEM-05)
 * and the follow switch. `cardRef` is where focus goes when the action it was on gives way. The
 * scene page shares it (spec 0008 Q8 A) with its own `withdraw`.
 */
export function RequestCard({
  target,
  withdraw,
  headingLevel: Heading = 'h3',
  state,
  note,
  journeyTitle,
  journeys,
  albums,
  action,
  openIn = [],
  openInPrimary = false,
  outcome,
  cardRef,
  children,
}: {
  /** The title whose waiting requests the card offers to withdraw (member requests). */
  target?: TitleRef
  /** Replaces the member withdraw action (the scene page withdraws its private request). */
  withdraw?: ReactNode
  /** The card's (hidden) heading level: h3 inside the overlay, h2 on the scene page. */
  headingLevel?: 'h2' | 'h3'
  state: StateView | null
  note?: string | null
  journeyTitle: string
  journeys: UnitJourney[]
  albums?: Record<string, string>
  /** The request action, if one applies now. */
  action?: ReactNode
  openIn?: OpenInLink[]
  /** "Open in" is the card's (and the view's) one gold action. */
  openInPrimary?: boolean
  /** What the last request did, announced politely. */
  outcome?: ReactNode
  cardRef: Ref<HTMLElement>
  children?: ReactNode
}) {
  const { t } = useTranslation('member')
  const headingId = useId()
  return (
    <section
      ref={cardRef}
      tabIndex={-1}
      aria-labelledby={headingId}
      // The follow switch drops its divider when nothing sits above it (heading and live region).
      className="flex flex-col gap-4 rounded-panel border border-line bg-night-0/60 p-5 outline-none [&>[data-follow]:nth-child(3)]:border-t-0 [&>[data-follow]:nth-child(3)]:pt-0"
    >
      <Heading id={headingId} className="sr-only">
        {t('detail.requestCard')}
      </Heading>
      {(state || note) && (
        <div className="flex flex-col items-start gap-2">
          {state && <StateChip state={state} />}
          {note && <p className="text-sm text-text-2">{note}</p>}
        </div>
      )}
      {(action || openIn.length > 0) && (
        <div className="flex flex-col gap-2">
          {action}
          {openIn.map((link, i) => (
            <Button
              key={link.url}
              asChild
              variant={openInPrimary && i === 0 ? 'primary' : 'secondary'}
              className="w-full"
            >
              <a
                href={link.url}
                target="_blank"
                rel="noopener noreferrer"
                title={t('title.openInHint', { name: link.name })}
              >
                <ExternalLinkIcon aria-hidden />
                {t('title.openIn', { service: serviceName(link.service) })}
              </a>
            </Button>
          ))}
        </div>
      )}
      <div aria-live="polite" className="empty:hidden">
        {outcome}
      </div>
      {journeys.length > 0 && (
        <div className="border-t border-line pt-4">
          <JourneyCard
            title={journeyTitle}
            units={journeys}
            albums={albums}
            layout="column"
            heading={false}
          />
        </div>
      )}
      {journeys.length > 0 && (withdraw ?? (target && <Withdraw target={target} />))}
      {children}
    </section>
  )
}

/** "Withdraw request" for each of the member's requests still waiting for approval (MEM-05). */
function Withdraw({ target }: { target: TitleRef }) {
  const { t } = useTranslation('member')
  const mine = useMyRequests()
  const withdraw = useWithdrawRequest()
  const errorMessage = useApiErrorMessage()
  const list = withdrawables(mine.data ?? [], target)
  if (list.length === 0) return null
  const label = (w: (typeof list)[number]) =>
    list.length === 1
      ? t('detail.withdraw')
      : w.seasons.length > 0
        ? t('detail.withdrawSeasons', {
            count: w.seasons.length,
            seasons: w.seasons.map((n) => formatNumber(n)).join(', '),
          })
        : t('detail.withdrawAlbums', { albums: w.albums.join(', ') })
  return (
    <div className="-mx-1 flex flex-col items-start gap-1">
      {list.map((w) => (
        <Button
          key={w.id}
          size="sm"
          variant="ghost"
          aria-disabled={withdraw.isPending || undefined}
          onClick={(event) => {
            if (withdraw.isPending) return
            const hadFocus = event.currentTarget === document.activeElement
            // The card itself, found now: after the refetch this button may be gone.
            const card = event.currentTarget.closest('section')
            // Not per-call callbacks: they never fire once the refetch unmounts this list.
            withdraw
              .mutateAsync(w.id)
              .then(() => {
                toast(t('requests.withdrawn'))
                // The button goes; focus stays in the card, never on <body>.
                if (hadFocus && card?.isConnected) focusElement(card, { quiet: true })
              })
              .catch((error: unknown) => toast.error(errorMessage(error)))
          }}
        >
          {label(w)}
        </Button>
      ))}
    </div>
  )
}

/** The trailer: a ghost link after the request card (MEM-08), leaving for YouTube. */
export function TrailerLink({ url }: { url: string }) {
  const { t } = useTranslation('member')
  return (
    <Button asChild variant="ghost" className="self-start">
      <a href={url} target="_blank" rel="noopener noreferrer" title={t('title.trailerHint')}>
        <PlayIcon aria-hidden />
        {t('detail.trailer')}
      </a>
    </Button>
  )
}

/**
 * The one follow control (MEM-07): a switch for a series' new seasons or an artist's new
 * albums. `ended` softens it: hidden for an ended series nobody follows, a note when followed.
 */
export function FollowSwitch({
  titleRef,
  title,
  ended = false,
}: {
  titleRef: TitleRef
  title: string
  ended?: boolean
}) {
  const { t } = useTranslation('member')
  const follows = useFollows()
  const follow = useFollow()
  const unfollow = useUnfollow()
  const errorMessage = useApiErrorMessage()
  const id = useId()
  const current = followOf(follows.data, titleRef)
  const context = titleRef.kind === 'artist' ? 'artist' : undefined
  if (ended && !current) return null
  const busy = follow.isPending || unfollow.isPending || follows.isPending
  const onError = (error: unknown) => toast.error(errorMessage(error))
  const hint = () => {
    if (ended) return t('detail.seriesEnded')
    if (!current) return t('follows.followHint', { context })
    if (current.knownThrough != null) {
      return t('follows.followingHint_artist', { date: formatDay(current.knownThrough) })
    }
    const cutoff = followCutoff(current)
    return cutoff === null
      ? t('follows.followingHintAll')
      : t('follows.followingHint', { season: cutoff })
  }
  return (
    <div data-follow className="flex items-start justify-between gap-4 border-t border-line pt-4">
      <div className="flex min-w-0 flex-col gap-0.5">
        <label htmlFor={id} className="cursor-pointer text-base font-medium text-text">
          {t('follows.followTitle', { context })}
        </label>
        <p id={`${id}-hint`} className="text-sm text-text-3">
          {hint()}
        </p>
      </div>
      {/* A 44 px target around the small switch (touch). */}
      <span className="-m-3 grid size-11 shrink-0 place-items-center">
        <Switch
          id={id}
          aria-describedby={`${id}-hint`}
          checked={current !== undefined}
          disabled={busy}
          onCheckedChange={(on) =>
            on
              ? follow.mutate(titleRef, {
                  onSuccess: () => toast(t('follows.followed', { title })),
                  onError,
                })
              : current &&
                unfollow.mutate(current.id, {
                  onSuccess: () => toast(t('follows.unfollowed', { title })),
                  onError,
                })
          }
        />
      </span>
    </div>
  )
}

/**
 * The bar under a selectable list (seasons, albums; MEM-06): a hint until something is chosen,
 * then the gold request action; "Everything is available" when nothing can be chosen. It sticks
 * to the bottom of the view while the list scrolls.
 */
export function SelectionBar({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div
      data-selection-bar
      tabIndex={-1}
      className="sticky bottom-0 z-10 -mx-1 mt-4 flex flex-wrap items-center justify-between gap-3 rounded-card border border-line bg-night-2 py-3 pr-3 pl-4 max-lg:bottom-3"
    >
      <div className="min-w-0 text-sm text-text-2">{children}</div>
      {action}
    </div>
  )
}
