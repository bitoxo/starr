import { ArrowRightIcon } from 'lucide-react'
import { type RefObject, useId, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'

import { useWithdrawRequest } from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import {
  type PrivateCard,
  type PrivateRef,
  type PrivateScene,
  useCreatePrivateRequests,
  usePrivateEntity,
  usePrivateRequests,
} from '@/api/private'
import { CategoryDot } from '@/components/CategoryDot'
import { RequestCard } from '@/components/detail/parts'
import { Cover } from '@/components/Poster'
import { EntityShape } from '@/components/private/EntityTile'
import { DESKTOP, useMediaQuery } from '@/components/shell/useSidebar'
import { Skeleton } from '@/components/Skeleton'
import { StateChip } from '@/components/StateChip'
import { Button } from '@/components/ui/button'
import { waitingForApproval } from '@/lib/detail'
import { formatDay, formatSeconds } from '@/lib/format'
import { coverLift } from '@/lib/motion'
import { privateToast } from '@/lib/privateGate'
import { entityPath, scenePath, useRequestToast } from '@/lib/privateNav'
import { focusElement } from '@/lib/returnFocus'
import { useRovingFocus } from '@/lib/roving'
import { availabilityState, cardState, stateLabel } from '@/lib/states'
import { cn } from '@/lib/utils'

/*
 * The scene page (spec 0008 Q8 A, W5, PRIV-05/06): a YouTube-like watch layout. The image takes
 * at most 60 % of the width on the left; below it the title, date · running time, the studio
 * (logo tile) and performers (round, A6) and the description in a collapsible box. A sticky side
 * column holds the request card with its journey (shared with the detail overlay, A7) and "More
 * from <studio>". Below 1024 px it is one column with the request card right under the title.
 * The scene's backdrop is the ambient layer behind the page (A1). Entrance as in the overlay:
 * the image settles, the text blocks rise one after the other, nothing moves with reduced motion.
 *
 * Privacy (spec 0007, amended by 0008 Q14): title and description are shown as text here only,
 * behind the gate; never in `document.title`, a URL, a toast or an `aria-live` region.
 */

const MEDIA_SIZES = '(min-width: 1024px) 60vw, 100vw'

export function SceneView({ scene }: { scene: PrivateScene }) {
  const { t } = useTranslation('member')
  const wide = useMediaQuery(DESKTOP)
  // Owned here, not by the card: the card moves between the columns at 1024 px, and a request
  // in flight (its pending state, its focus handling) must survive that move.
  const create = useCreatePrivateRequests()
  const card = useRef<HTMLElement>(null)
  const focusInCard = useRef(false)
  useLayoutEffect(() => {
    // The card moved with focus inside: focus follows it, never <body> (F3).
    if (focusInCard.current && card.current && !card.current.contains(document.activeElement))
      focusElement(card.current, { quiet: true })
  }, [wide])
  const request = (
    <div
      className="contents"
      onFocus={() => (focusInCard.current = true)}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) focusInCard.current = false
      }}
    >
      <SceneRequestCard scene={scene} create={create} cardRef={card} />
    </div>
  )
  const more = scene.moreFromStudio.some((s) => s.id !== scene.id) ? scene.studio : null
  const facts = [
    scene.date && formatDay(scene.date),
    scene.durationSeconds != null && formatSeconds(scene.durationSeconds),
  ].filter((f): f is string => Boolean(f))
  return (
    <article className="grid items-start gap-x-9 gap-y-6 lg:grid-cols-[minmax(0,3fr)_minmax(19rem,2fr)]">
      <div className="flex min-w-0 flex-col gap-5">
        <div className="detail-settle relative -mx-4 sm:mx-0">
          <Cover
            file={scene.image}
            title={scene.title}
            sizes={MEDIA_SIZES}
            category="adult"
            className="max-sm:rounded-none sm:shadow-[0_24px_50px_rgb(0_0_0/0.4)]"
          />
          {scene.durationSeconds != null && (
            <span
              aria-hidden
              className="type-data absolute right-3 bottom-3 rounded-[5px] bg-night-0/85 px-2 py-0.5 text-xs text-text"
            >
              {formatSeconds(scene.durationSeconds)}
            </span>
          )}
        </div>
        <div className="detail-stagger flex min-w-0 flex-col gap-5">
          <div className="flex flex-col gap-2.5">
            <h1 className="type-display text-3xl leading-tight font-extrabold break-words text-text sm:text-4xl">
              {scene.title}
            </h1>
            <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-text-2">
              {facts.map((fact, i) => (
                <span key={fact} className="flex items-center gap-3">
                  {i > 0 && <span aria-hidden>·</span>}
                  <span className="type-data text-text">{fact}</span>
                </span>
              ))}
              <span className="flex items-center gap-2">
                <CategoryDot category="adult" className="size-2" />
                {t('private.scene')}
              </span>
            </p>
          </div>
          {!wide && request}
          {scene.studio && <StudioLink studio={scene.studio} />}
          {scene.performers.length > 0 && <Performers performers={scene.performers} />}
          {scene.details && <Description text={scene.details} />}
        </div>
      </div>
      {(wide || more) && (
        <aside
          // Named by what it holds: the card (desktop only) and "More from <studio>".
          aria-label={
            !more
              ? t('detail.requestCard')
              : wide
                ? t('private.sceneSide')
                : t('private.moreFrom', { name: more.name })
          }
          className="scene-side flex min-w-0 flex-col gap-8 lg:sticky lg:top-6"
        >
          {wide && request}
          {more && (
            <MoreFromStudio studio={more} scenes={scene.moreFromStudio} current={scene.id} />
          )}
        </aside>
      )}
    </article>
  )
}

/** The page's shape while the scene loads: image, title, facts; the side card beside it. */
export function SceneSkeleton() {
  return (
    <div
      aria-busy
      className="grid items-start gap-x-9 gap-y-6 lg:grid-cols-[minmax(0,3fr)_minmax(19rem,2fr)]"
    >
      <div className="flex flex-col gap-5">
        <Skeleton className="-mx-4 aspect-video rounded-none sm:mx-0 sm:rounded-card" />
        <Skeleton className="h-9 w-2/3 rounded-full" />
        <Skeleton className="h-4 w-48 rounded-full" />
      </div>
      <Skeleton className="h-32 rounded-panel max-lg:hidden" />
    </div>
  )
}

/** The request card of the detail overlay (A7), with the scene's copy and private toasts. */
function SceneRequestCard({
  scene,
  create,
  cardRef: card,
}: {
  scene: PrivateScene
  create: ReturnType<typeof useCreatePrivateRequests>
  cardRef: RefObject<HTMLElement | null>
}) {
  const { t } = useTranslation('member')
  const errorMessage = useApiErrorMessage()
  const toastCreated = useRequestToast()
  const { status, requestable, journey, openIn, requestedByMe } = scene
  const journeys = journey ? [{ journey }] : []
  const canRequest = requestable && status === 'new'
  const note =
    status === 'available'
      ? openIn.length === 0
        ? t('private.isAvailable')
        : t('private.openInHint')
      : !requestable
        ? status === 'new'
          ? t('private.requestsClosed')
          : null
        : canRequest
          ? t('detail.requestHint')
          : status === 'requested' && !requestedByMe && !journey
            ? t('private.requestedByOthers')
            : null
  return (
    <RequestCard
      cardRef={card}
      headingLevel="h2"
      state={cardState({ status, requestedByMe, journeys })}
      note={note}
      journeyTitle={scene.title}
      journeys={journeys}
      openIn={openIn}
      openInPrimary={!canRequest}
      withdraw={<SceneWithdraw id={scene.id} />}
      action={
        canRequest ? (
          <Button
            variant="primary"
            className="w-full"
            aria-disabled={create.isPending || undefined}
            onClick={(event) => {
              if (create.isPending) return
              const hadFocus = event.currentTarget === document.activeElement
              create.mutate([scene.id], {
                onSuccess: (created) => {
                  toastCreated(created)
                  // The button gives way: focus stays in the card, never on <body> (F3).
                  if (hadFocus && card.current?.isConnected)
                    focusElement(card.current, { quiet: true })
                },
                onError: (error) => privateToast(errorMessage(error), {}, 'error'),
              })
            }}
          >
            {t('private.requestScene')}
          </Button>
        ) : null
      }
    />
  )
}

/** "Withdraw request" while the member's request for this scene waits for approval (MEM-05). */
function SceneWithdraw({ id }: { id: string }) {
  const { t } = useTranslation('member')
  const requests = usePrivateRequests(false)
  const withdraw = useWithdrawRequest()
  const errorMessage = useApiErrorMessage()
  const mine = (requests.data ?? []).find((r) => r.titleId === id && waitingForApproval(r))
  if (!mine) return null
  return (
    <div className="-mx-1 flex flex-col items-start">
      <Button
        size="sm"
        variant="ghost"
        aria-disabled={withdraw.isPending || undefined}
        onClick={(event) => {
          if (withdraw.isPending) return
          const hadFocus = event.currentTarget === document.activeElement
          const card = event.currentTarget.closest('section')
          withdraw
            .mutateAsync(mine.id)
            .then(() => {
              privateToast(t('privateRequests.withdrawn'))
              if (hadFocus && card?.isConnected) focusElement(card, { quiet: true })
            })
            .catch((error: unknown) => privateToast(errorMessage(error), {}, 'error'))
        }}
      >
        {t('detail.withdraw')}
      </Button>
    </div>
  )
}

/** The studio as its 16:9 logo tile (A6) with its name; the logo comes from its page. */
function StudioLink({ studio }: { studio: PrivateRef }) {
  const { t } = useTranslation('member')
  // The studio page's first page is what "More from" was built from: the server has it cached.
  const page = usePrivateEntity('studio', studio.id)
  return (
    <Link
      to={entityPath('studio', studio.id)}
      className="group flex items-center gap-3 self-start rounded-card pr-2"
    >
      <span className="w-24 shrink-0 rounded-card ring-polaris ring-offset-2 ring-offset-night-0 group-hover:ring-2 group-focus-visible:ring-2">
        <EntityShape kind="studio" image={page.data?.image} name={studio.name} compact />
      </span>
      <span className="flex min-w-0 flex-col">
        <span aria-hidden className="type-data text-xs text-text-3">
          {t('private.studio')}
        </span>
        <span className="truncate text-base font-semibold text-text group-hover:underline group-hover:underline-offset-4">
          {studio.name}
        </span>
      </span>
    </Link>
  )
}

/** The performers, round (A6), each a link to their page. */
function Performers({ performers }: { performers: PrivateRef[] }) {
  const { t } = useTranslation('member')
  const id = useId()
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2.5">
      <h2 id={id} className="type-data text-xs text-text-3">
        {t('private.performers')}
      </h2>
      <ul className="flex flex-wrap gap-x-5 gap-y-3">
        {performers.map((p) => (
          <li key={p.id}>
            <Link
              to={entityPath('performer', p.id)}
              className="group flex items-center gap-2.5 rounded-full pr-2"
            >
              <span className="w-10 shrink-0 rounded-full text-[9px] ring-polaris ring-offset-2 ring-offset-night-0 group-hover:ring-2 group-focus-visible:ring-2">
                <EntityShape kind="performer" name={p.name} />
              </span>
              <span className="text-base text-text group-hover:underline group-hover:underline-offset-4">
                {p.name}
              </span>
            </Link>
          </li>
        ))}
      </ul>
    </section>
  )
}

/**
 * The description (spec 0008 Q14): plain text in a box, three lines until "Show more" (shown
 * only when the text is longer than that). Paragraphs follow the text's blank lines.
 */
function Description({ text }: { text: string }) {
  const { t } = useTranslation('member')
  const id = useId()
  const body = useRef<HTMLDivElement>(null)
  const [open, setOpen] = useState(false)
  const [overflows, setOverflows] = useState(false)
  useLayoutEffect(() => {
    const el = body.current
    if (!el || open) return
    const measure = () => setOverflows(el.scrollHeight > el.clientHeight + 1)
    measure()
    if (typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(measure)
    observer.observe(el)
    return () => observer.disconnect()
  }, [open, text])
  const paragraphs = text.split(/\n\s*\n/).filter((p) => p.trim())
  return (
    <section
      aria-labelledby={`${id}-h`}
      className="rounded-panel border border-line bg-night-1 px-5 py-4"
    >
      <h2 id={`${id}-h`} className="mb-1.5 text-sm font-semibold text-text-3">
        {t('private.description_heading')}
      </h2>
      <div
        ref={body}
        id={`${id}-body`}
        className={cn(
          'text-base leading-relaxed whitespace-pre-line text-text [&>p+p]:mt-2.5',
          !open && 'line-clamp-3',
        )}
      >
        {paragraphs.map((p, i) => (
          <p key={i}>{p.trim()}</p>
        ))}
      </div>
      {(overflows || open) && (
        <button
          type="button"
          aria-expanded={open}
          aria-controls={`${id}-body`}
          onClick={() => setOpen(!open)}
          className="mt-2 -ml-1 min-h-6 cursor-pointer rounded-control px-1 text-sm font-semibold text-text-2 hover:text-text max-lg:min-h-11"
        >
          {open ? t('private.showLess') : t('private.showMore')}
        </button>
      )}
    </section>
  )
}

/** "More from <studio>": the studio's newest other scenes as a list; one tab stop (F4). */
function MoreFromStudio({
  studio,
  scenes,
  current,
}: {
  studio: PrivateRef
  scenes: PrivateCard[]
  current: string
}) {
  const { t } = useTranslation('member')
  const id = useId()
  const shown = scenes.filter((s) => s.id !== current)
  const { itemProps, onKeyDown } = useRovingFocus<HTMLAnchorElement>(shown.length, { grid: true })
  if (shown.length === 0) return null
  return (
    <section aria-labelledby={id} className="flex flex-col gap-3">
      <div className="flex items-baseline justify-between gap-3">
        <h2 id={id} className="type-display min-w-0 text-xl font-bold break-words text-text">
          {t('private.moreFrom', { name: studio.name })}
        </h2>
        <Link
          to={entityPath('studio', studio.id)}
          className="inline-flex min-h-6 shrink-0 items-center gap-1 rounded-control text-sm text-text-2 hover:text-text max-lg:min-h-11"
        >
          {t('private.allScenes')}
          <ArrowRightIcon aria-hidden className="size-4" />
        </Link>
      </div>
      <ul aria-labelledby={id} onKeyDown={onKeyDown} className="-mx-1.5 flex flex-col gap-1">
        {shown.map((card, index) => (
          <li key={card.id}>
            <RelatedScene card={card} roving={itemProps(index)} />
          </li>
        ))}
      </ul>
    </section>
  )
}

function RelatedScene({
  card,
  roving,
}: {
  card: PrivateCard
  roving: ReturnType<ReturnType<typeof useRovingFocus<HTMLAnchorElement>>['itemProps']>
}) {
  const state = card.status ? availabilityState(card.status, card.requestedByMe) : null
  const performers = card.performers.map((p) => p.name).join(', ')
  const label = [
    card.title,
    performers,
    card.date && formatDay(card.date),
    card.durationSeconds != null && formatSeconds(card.durationSeconds),
    state && stateLabel(state),
  ]
    .filter(Boolean)
    .join(', ')
  return (
    <Link
      to={scenePath(card.id)}
      {...roving}
      aria-label={label}
      className="group grid grid-cols-[9rem_minmax(0,1fr)] items-start gap-3 rounded-card p-1.5 hover:bg-night-1/70 focus-visible:outline-none sm:grid-cols-[10.5rem_minmax(0,1fr)]"
    >
      <span
        className={cn(
          'relative block overflow-hidden rounded-control ring-polaris ring-offset-2 ring-offset-night-0 group-hover:ring-2 group-focus-visible:ring-2',
          coverLift(),
        )}
      >
        <Cover file={card.image} title={card.title} sizes="168px" category="adult" />
        {card.durationSeconds != null && (
          <span className="type-data absolute right-1.5 bottom-1.5 rounded-[4px] bg-night-0/85 px-1 py-px text-[11px] text-text">
            {formatSeconds(card.durationSeconds)}
          </span>
        )}
      </span>
      <span aria-hidden className="flex min-w-0 flex-col gap-0.5 pt-0.5">
        <span
          title={card.title}
          className="line-clamp-2 text-sm leading-snug font-semibold text-text"
        >
          {card.title}
        </span>
        {performers && <span className="truncate text-sm text-text-2">{performers}</span>}
        {card.date && <span className="type-data text-xs text-text-3">{formatDay(card.date)}</span>}
        {state && <StateChip state={state} className="mt-1 self-start" />}
      </span>
    </Link>
  )
}
