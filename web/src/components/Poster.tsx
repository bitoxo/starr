import { type Ref, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { type CategoryName, categoryOfKind, coverImage, isMusic, tmdbImage } from '@/api/categories'
import { privateImage } from '@/api/private'
import type { MediaKind, TitleCard } from '@/api/discover'
import { CategoryDot } from '@/components/CategoryDot'
import { CoverFallback } from '@/components/CoverFallback'
import { AvailabilityChip } from '@/components/RequestStatus'
import { StateChip } from '@/components/StateChip'
import { JourneyChip } from '@/components/StatusJourney'
import { useJourneyTitle } from '@/lib/journey'
import { coverLift } from '@/lib/motion'
import { useInPrivateArea } from '@/lib/privateArea'
import { cardState, stateLabel } from '@/lib/states'
import { cn } from '@/lib/utils'

/**
 * A cover through Starr's image proxy in its category's format: 2:3 for movies and series
 * (TMDB), 1:1 for albums and artists (Cover Art Archive; artists round), 16:9 for the private
 * category (its own proxy source, only inside `PrivateArea`). The one fallback tile stands in
 * while there is none, also when the archive has no cover for an album (MEM-25).
 */
export function Cover({
  file,
  title,
  sizes,
  kind = 'movie',
  category,
  className,
}: {
  file?: string | null
  title: string
  /** `sizes` attribute for picking the image width. */
  sizes: string
  kind?: MediaKind
  /** The title's category, where known; private covers render only in the private area. */
  category?: CategoryName
  className?: string
}) {
  const music = isMusic(kind)
  const inPrivateArea = useInPrivateArea()
  // The archive answers 404 for albums without a cover; remember it per file.
  const [failed, setFailed] = useState<string | null>(null)
  const shape = cn(
    category === 'adult' ? 'aspect-video' : music ? 'aspect-square' : 'aspect-2/3',
    kind === 'artist' ? 'rounded-full' : 'rounded-card',
  )
  if (category === 'adult' && !inPrivateArea) {
    // Never a private cover outside its area, not even blurred, and no title either.
    return (
      <span
        aria-hidden
        data-private-cover
        className={cn('block w-full border border-line bg-night-1', shape, className)}
      />
    )
  }
  if (!file || failed === file) {
    return (
      <CoverFallback
        title={title}
        category={category ?? categoryOfKind(kind)}
        compact={category === 'adult'}
        className={cn(shape, className)}
      />
    )
  }
  const [src, srcSet] =
    category === 'adult'
      ? // Inside the area, only through the private source (spec 0007 Q6), never another.
        [
          privateImage(file, '600'),
          `${privateImage(file, '300')} 300w, ${privateImage(file, '600')} 600w, ${privateImage(file, '1280')} 1280w`,
        ]
      : music
        ? [
            coverImage(file, '500'),
            `${coverImage(file, '250')} 250w, ${coverImage(file, '500')} 500w`,
          ]
        : [
            tmdbImage(file, 'w342'),
            `${tmdbImage(file, 'w185')} 185w, ${tmdbImage(file, 'w342')} 342w, ${tmdbImage(file, 'w500')} 500w`,
          ]
  return (
    <img
      src={src}
      srcSet={srcSet}
      sizes={sizes}
      alt=""
      loading="lazy"
      decoding="async"
      onError={() => setFailed(file)}
      className={cn('w-full bg-night-2 object-cover', shape, className)}
    />
  )
}

/** Roving-tabindex props a lane hands each poster (`useRovingFocus().itemProps`). */
export type RovingProps = {
  ref: Ref<HTMLButtonElement>
  tabIndex: number
  onFocus: () => void
}

/**
 * A title in a swimlane or result grid. The whole tile is one button that opens the detail
 * view. Focus and hover lift it with the Polaris ring (the lift only with motion allowed,
 * MEM-15). In a lane (`roving`) it is the cover alone, without a caption (spec 0008 A3): one
 * stop of the lane's roving tabindex named "Title (Year), state", with the chip on the cover not
 * a control of its own, so a lane stays one tab stop (MEM-04). In a grid it keeps a caption for
 * scanning (title, artist, category marker, year; MEM-17) and the chip opens the journey.
 */
export function Poster({
  card,
  onOpen,
  roving,
  caption = !roving,
  prefix,
  detail,
  sizes = '(min-width: 1024px) 176px, 128px',
  className,
}: {
  card: TitleCard
  onOpen: (card: TitleCard) => void
  roving?: RovingProps
  /** The caption under the cover; grids keep it, also when they rove ("See all"). */
  caption?: boolean
  /** Put before a roving cover's name, e.g. "Number 11" in a ranked lane. */
  prefix?: string
  /** The caption's last fact instead of the year, e.g. an exact release day. */
  detail?: string
  sizes?: string
  className?: string
}) {
  const { t } = useTranslation('member')
  const journeyTitle = useJourneyTitle()
  const state = cardState(card)
  const lift = coverLift('peer')
  const name = roving
    ? [
        prefix,
        card.year != null ? `${card.title} (${card.year})` : card.title,
        card.artist,
        state && stateLabel(state),
      ]
        .filter(Boolean)
        .join(', ')
    : undefined
  return (
    <div className={cn('group relative', className)}>
      <button
        type="button"
        {...roving}
        aria-label={name}
        onClick={(e) => {
          // Safari does not focus a clicked button; the detail view returns focus here.
          e.currentTarget.focus({ preventScroll: true })
          onOpen(card)
        }}
        className="peer flex w-full cursor-pointer flex-col gap-2 rounded-card text-left focus-visible:outline-none"
      >
        <span
          className={cn(
            'relative block w-full ring-polaris ring-offset-2 ring-offset-night-0 group-has-[.peer:focus-visible]:ring-2',
            card.kind === 'artist' ? 'rounded-full' : 'rounded-card',
            lift,
          )}
        >
          <Cover file={card.poster} title={card.title} sizes={sizes} kind={card.kind} />
        </span>
        {caption && (
          // Grids are for scanning: title, artist, then the category marker and year (MEM-17).
          <span className="flex min-w-0 flex-col px-0.5">
            <span
              title={card.title}
              className="line-clamp-2 text-sm leading-snug font-medium text-text"
            >
              {card.title}
            </span>
            {card.artist && (
              <span title={card.artist} className="truncate text-xs text-text-2">
                {card.artist}
              </span>
            )}
            <span className="mt-0.5 flex items-center gap-1.5 text-xs text-text-3">
              <CategoryDot category={categoryOfKind(card.kind)} className="size-1.5" />
              <span>{t(`discover.kind.${card.kind}`)}</span>
              {(detail ?? card.year) != null && (
                <>
                  <span aria-hidden>·</span>
                  <span className="type-data">{detail ?? card.year}</span>
                </>
              )}
            </span>
          </span>
        )}
      </button>
      {state && (
        // Outside the tile's button (no button in a button), laid over the cover.
        <span
          className={cn(
            'pointer-events-none absolute inset-x-0 top-0',
            isMusic(card.kind) ? 'aspect-square' : 'aspect-2/3',
            lift,
          )}
        >
          {/* Dark backing so the tinted chip reads on any cover. */}
          <span className="pointer-events-auto absolute bottom-2 left-2 max-w-[calc(100%-1rem)] overflow-hidden rounded-full bg-night-0/90">
            {roving ? (
              <span aria-hidden>
                <StateChip state={state} className="text-xs" />
              </span>
            ) : (
              <JourneyChip title={journeyTitle(card.title, card.kind)} units={card.journeys}>
                <AvailabilityChip
                  units={card.journeys}
                  status={card.status}
                  requestedByMe={card.requestedByMe}
                  className="text-xs"
                />
              </JourneyChip>
            )}
          </span>
        </span>
      )}
    </div>
  )
}
