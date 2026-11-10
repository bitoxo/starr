import { CheckIcon, PlusIcon } from 'lucide-react'
import type { KeyboardEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'

import { useApiErrorMessage } from '@/api/errors'
import {
  type CreatedPrivateRequests,
  MAX_SCENES,
  type PrivateCard,
  useCreatePrivateRequests,
} from '@/api/private'
import { Cover } from '@/components/Poster'
import { Skeleton } from '@/components/Skeleton'
import { StateChip } from '@/components/StateChip'
import { Button } from '@/components/ui/button'
import { formatDay, formatNumber, formatSeconds } from '@/lib/format'
import { coverLift } from '@/lib/motion'
import { privateToast } from '@/lib/privateGate'
import { scenePath, useRequestToast } from '@/lib/privateNav'
import { useRovingFocus } from '@/lib/roving'
import type { SceneSelection } from '@/lib/sceneSelection'
import { availabilityState, stateLabel } from '@/lib/states'
import { cn } from '@/lib/utils'

/*
 * The private scene grid (spec 0008 Q7 A, F4, PRIV-18): large 16:9 thumbnails with a duration
 * badge and the state chip, a caption below (title, studio · performers, date). The grid is one
 * tab stop (roving tabindex in two dimensions). Where scenes can be requested, each new scene
 * has a checkbox on its thumbnail, first in the DOM; Space on a focused scene toggles it, Esc
 * clears the selection, and a sticky bar says "n / 20" and requests them together. Render only
 * inside `PrivateArea`.
 */

export const SCENE_GRID =
  'grid grid-cols-1 gap-x-5 gap-y-8 min-[560px]:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4'
const SIZES = '(min-width: 1536px) 25vw, (min-width: 1024px) 33vw, (min-width: 560px) 50vw, 100vw'

/** What the caption's second line names: the studio and performers, or performers only. */
export type CaptionMode = 'studio-performers' | 'performers'

function pickable(card: PrivateCard): boolean {
  return card.kind === 'scene' && (card.status ?? 'new') === 'new'
}

function captionLine(card: PrivateCard, mode: CaptionMode): string {
  const performers = (card.performers ?? []).map((p) => p.name)
  const parts =
    mode === 'performers' ? [performers.join(', ')] : [card.studio?.name, performers.join(', ')]
  return parts.filter(Boolean).join(' · ')
}

/** The link's accessible name: title, studio, date, running time, state. */
function sceneLabel(card: PrivateCard, mode: CaptionMode, selected: string | null) {
  const state = card.status ? availabilityState(card.status, card.requestedByMe) : null
  return [
    card.title,
    captionLine(card, mode),
    card.date && formatDay(card.date),
    card.durationSeconds != null && formatSeconds(card.durationSeconds),
    state && stateLabel(state),
    selected,
  ]
    .filter(Boolean)
    .join(', ')
}

function SceneCard({
  card,
  mode,
  selectable,
  selected,
  onToggle,
  roving,
}: {
  card: PrivateCard
  mode: CaptionMode
  selectable: boolean
  selected: boolean
  onToggle: () => void
  roving: ReturnType<ReturnType<typeof useRovingFocus<HTMLAnchorElement>>['itemProps']>
}) {
  const { t } = useTranslation('member')
  const canPick = selectable && pickable(card)
  const state = card.status ? availabilityState(card.status, card.requestedByMe) : null
  const line = captionLine(card, mode)
  const onKeyDown = (event: KeyboardEvent<HTMLAnchorElement>) => {
    if (event.key !== ' ' || event.altKey || event.ctrlKey || event.metaKey) return
    event.preventDefault()
    if (canPick) onToggle()
  }
  return (
    <div className="group/scene relative min-w-0" data-selected={selected || undefined}>
      {canPick && (
        // First in the DOM (PRIV-18); not a tab stop of its own: Space on the scene toggles it.
        <button
          type="button"
          role="checkbox"
          tabIndex={-1}
          aria-checked={selected}
          aria-label={t('private.select', { title: card.title })}
          onClick={onToggle}
          className={cn(
            'absolute top-2 left-2 z-10 grid size-9 cursor-pointer max-lg:size-11 place-items-center rounded-control bg-night-0/75 transition-opacity duration-(--dur-1)',
            selected
              ? 'opacity-100'
              : 'opacity-0 group-focus-within/scene:opacity-100 group-hover/scene:opacity-100 group-data-[selecting]/grid:opacity-100 [@media(hover:none)]:opacity-100',
          )}
        >
          <span
            className={cn(
              'grid size-5 place-items-center rounded-[5px] border-[1.5px]',
              selected
                ? 'border-polaris bg-polaris text-polaris-ink'
                : 'border-text text-transparent',
            )}
          >
            <CheckIcon aria-hidden className="size-3.5" strokeWidth={3} />
          </span>
        </button>
      )}
      <Link
        to={scenePath(card.id)}
        {...roving}
        onKeyDown={onKeyDown}
        aria-label={sceneLabel(card, mode, selected ? t('private.selected') : null)}
        className="group block rounded-card focus-visible:outline-none"
      >
        <span
          className={cn(
            'relative block overflow-hidden rounded-card ring-polaris ring-offset-2 ring-offset-night-0 group-hover:ring-2 group-focus-visible:ring-2',
            coverLift(),
          )}
        >
          <Cover file={card.image} title={card.title} sizes={SIZES} category="adult" />
          {selected && (
            <span
              aria-hidden
              className="absolute inset-0 rounded-card bg-night-0/35 shadow-[inset_0_0_0_3px_var(--color-polaris)]"
            />
          )}
          {state && (
            <StateChip
              state={state}
              className="absolute bottom-2 left-2 max-w-[calc(100%-6rem)] overflow-hidden bg-night-0/85"
            />
          )}
          {card.durationSeconds != null && (
            <span className="type-data absolute right-2 bottom-2 rounded-[5px] bg-night-0/85 px-1.5 py-0.5 text-xs text-text">
              {formatSeconds(card.durationSeconds)}
            </span>
          )}
        </span>
      </Link>
      <div aria-hidden className="mt-3 flex min-w-0 flex-col gap-0.5 px-0.5">
        <span
          title={card.title}
          className="line-clamp-2 text-base leading-snug font-semibold text-text"
        >
          {card.title}
        </span>
        {line && <span className="truncate text-sm text-text-2">{line}</span>}
        {card.date && <span className="type-data text-xs text-text-3">{formatDay(card.date)}</span>}
      </div>
    </div>
  )
}

/** Loading tiles in the final shape: 16:9 thumbnail, title and facts lines (PRIV-24). */
export function SceneSkeletons({ count }: { count: number }) {
  return Array.from({ length: count }, (_, i) => (
    <li key={i} aria-hidden>
      <Skeleton className="aspect-video w-full rounded-card" />
      <Skeleton className="mt-3 h-4 w-3/4 rounded-full" />
      <Skeleton className="mt-2 h-3 w-1/2 rounded-full" />
    </li>
  ))
}

/**
 * The grid itself. `selection` is given where scenes can be requested (the category takes
 * requests); `loading` appends skeleton tiles (first load or the next page).
 */
export function SceneGrid({
  cards,
  label,
  mode = 'studio-performers',
  selection,
  loading = 0,
}: {
  cards: PrivateCard[]
  /** The id of the heading that names the grid. */
  label: string
  mode?: CaptionMode
  selection?: SceneSelection
  loading?: number
}) {
  const { itemProps, onKeyDown } = useRovingFocus<HTMLAnchorElement>(cards.length, { grid: true })
  return (
    <ul
      aria-labelledby={label}
      aria-busy={loading > 0 || undefined}
      onKeyDown={onKeyDown}
      data-selecting={(selection?.selected.length ?? 0) > 0 || undefined}
      className={cn('group/grid scroll-mt-24 scroll-pb-28', SCENE_GRID)}
    >
      {cards.map((card, index) => (
        <li key={card.id}>
          <SceneCard
            card={card}
            mode={mode}
            selectable={selection !== undefined}
            selected={selection?.selected.includes(card.id) ?? false}
            onToggle={() => selection?.toggle(card.id)}
            roving={itemProps(index)}
          />
        </li>
      ))}
      {loading > 0 && <SceneSkeletons count={loading} />}
    </ul>
  )
}

/** The bar goes away with its buttons: focus the grid's current scene, else its heading (F3). */
function focusGrid(heading: string) {
  const target =
    document.querySelector<HTMLElement>(`ul[aria-labelledby="${heading}"] a[tabindex="0"]`) ??
    document.getElementById(heading)
  target?.focus()
}

/** The sticky bar of a selection: "n / 20 selected", Clear, and the one request. */
export function SelectionBar({
  selection,
  grid,
}: {
  selection: SceneSelection
  /** The id of the grid's heading: focus goes back to the grid when the bar goes. */
  grid: string
}) {
  const { t } = useTranslation('member')
  const create = useCreatePrivateRequests()
  const errorMessage = useApiErrorMessage()
  const toastCreated = useRequestToast()
  const count = selection.selected.length
  if (count === 0) return null
  return (
    <div
      role="region"
      aria-label={t('private.selection', { count })}
      className="sticky bottom-20 z-20 mt-8 flex flex-wrap items-center gap-3 rounded-card border border-line bg-night-2 py-2.5 pr-2.5 pl-5 lg:bottom-4"
    >
      <p className="type-data text-sm text-text">
        {formatNumber(count)}
        <span className="text-text-3"> / {t('private.selectionOf', { max: MAX_SCENES })}</span>
      </p>
      <p className="flex-1 text-sm text-text-3 max-sm:hidden">{t('private.selectionKeys')}</p>
      <div className="flex gap-2 max-sm:w-full">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => {
            focusGrid(grid)
            selection.clear()
          }}
        >
          {t('private.clearSelection')}
        </Button>
        <Button
          variant="primary"
          size="sm"
          className="max-sm:flex-1"
          disabled={create.isPending}
          onClick={() =>
            create.mutate(selection.selected, {
              onSuccess: (created: CreatedPrivateRequests) => {
                toastCreated(created)
                focusGrid(grid)
                selection.clear()
              },
              onError: (error) => privateToast(errorMessage(error), {}, 'error'),
            })
          }
        >
          <PlusIcon aria-hidden />
          {t('private.requestSelected', { count })}
        </Button>
      </div>
    </div>
  )
}
