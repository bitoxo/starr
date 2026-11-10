import { useTranslation } from 'react-i18next'

import { categoryOfKind, isMusic, tmdbImage } from '@/api/categories'
import type { TitleCard } from '@/api/discover'
import { CoverFallback } from '@/components/CoverFallback'
import { Cover, type RovingProps } from '@/components/Poster'
import { StateChip } from '@/components/StateChip'
import { formatUpcomingDay } from '@/lib/format'
import { coverLift } from '@/lib/motion'
import { cardState, stateLabel } from '@/lib/states'
import { cn } from '@/lib/utils'

/**
 * A title of an "Upcoming" or "Out soon" lane (spec 0008 Q6 A, F7): its exact day on the image
 * ("Fri, Nov 13"). Movies and series show their 16:9 backdrop, albums their cover. A backdrop
 * carries no title art, so those cards keep a caption (title, then genres); an album cover does,
 * so albums stay captionless like every lane cover (A3). The state chip sits on the image as on
 * every cover. One stop of the lane's roving tabindex.
 */
export function DatedCard({
  card,
  onOpen,
  roving,
}: {
  card: TitleCard
  onOpen: (card: TitleCard) => void
  roving: RovingProps
}) {
  const { t } = useTranslation('member')
  const state = cardState(card)
  const day = card.releaseDate ? formatUpcomingDay(card.releaseDate) : null
  const music = isMusic(card.kind)
  const lift = coverLift('peer')
  const name = [card.title, card.artist, day, state && stateLabel(state)].filter(Boolean).join(', ')
  const sub = card.artist ?? card.genres.join(', ')
  return (
    <div className="group relative">
      <button
        type="button"
        {...roving}
        aria-label={name}
        onClick={(e) => {
          e.currentTarget.focus({ preventScroll: true })
          onOpen(card)
        }}
        className="peer flex w-full cursor-pointer flex-col gap-2.5 rounded-card text-left focus-visible:outline-none"
      >
        <span
          className={cn(
            'relative block w-full overflow-hidden rounded-card ring-polaris ring-offset-2 ring-offset-night-0 group-has-[.peer:focus-visible]:ring-2',
            lift,
          )}
        >
          {music ? (
            <Cover file={card.poster} title={card.title} sizes="176px" kind={card.kind} />
          ) : card.backdrop ? (
            <img
              src={tmdbImage(card.backdrop, 'w780')}
              srcSet={`${tmdbImage(card.backdrop, 'w300')} 300w, ${tmdbImage(card.backdrop, 'w780')} 780w`}
              sizes="288px"
              alt=""
              loading="lazy"
              decoding="async"
              className="aspect-video w-full bg-night-2 object-cover"
            />
          ) : (
            <CoverFallback
              title={card.title}
              category={categoryOfKind(card.kind)}
              compact
              className="aspect-video rounded-card"
            />
          )}
          {day && (
            <span
              aria-hidden
              data-day
              className="type-data absolute top-2 left-2 rounded-control bg-night-0 px-2 py-1 text-xs font-semibold text-text"
            >
              {day}
            </span>
          )}
          {state && (
            <span
              aria-hidden
              className="absolute bottom-2 left-2 max-w-[calc(100%-1rem)] overflow-hidden rounded-full bg-night-0/90"
            >
              <StateChip state={state} className="text-xs" />
            </span>
          )}
        </span>
        {!music && (
          <span aria-hidden className="flex min-w-0 flex-col px-0.5">
            <span
              title={card.title}
              className="type-display truncate text-base font-bold text-text"
            >
              {card.title}
            </span>
            {sub && <span className="truncate text-xs text-text-3">{sub}</span>}
            {!day && <span className="text-xs text-text-3">{t('category.dateUnknown')}</span>}
          </span>
        )}
      </button>
    </div>
  )
}
