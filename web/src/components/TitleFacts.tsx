import { StarIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import type { TitleCard, TitleDetails } from '@/api/discover'
import { formatMinutes, formatNumber } from '@/lib/format'
import { cn } from '@/lib/utils'

/**
 * A title's facts in one row (spec 0008 A3, A4): rating · year · runtime, seasons or artist ·
 * certification · genres, numbers in Geist Mono. A card carries rating, year, artist and genres;
 * runtime, seasons and certification come once the title's details are loaded.
 */
export function TitleFacts({
  card,
  details,
  genres = true,
  className,
}: {
  card: TitleCard
  details?: TitleDetails
  /** Leave the genres out (the preview card shows them on their own line). */
  genres?: boolean
  className?: string
}) {
  const { t } = useTranslation('member')
  const runtime =
    card.kind === 'movie' && details?.runtimeMinutes != null
      ? formatMinutes(details.runtimeMinutes)
      : null
  const seasons =
    card.kind === 'series' && details?.seasonCount != null
      ? t('discover.card.seasons', { count: details.seasonCount })
      : null
  const certification = details?.certification?.rating
  return (
    <p
      className={cn('flex flex-wrap items-center gap-x-3.5 gap-y-1 text-sm text-text-2', className)}
    >
      {card.rating != null && card.rating > 0 && (
        <span className="flex items-center gap-1">
          <StarIcon aria-hidden className="size-3.5 text-text-3" strokeWidth={1.75} />
          <span className="sr-only">{t('discover.card.rating')}</span>
          <span className="type-data">
            {formatNumber(card.rating, { minimumFractionDigits: 1, maximumFractionDigits: 1 })}
          </span>
        </span>
      )}
      {card.year != null && <span className="type-data">{card.year}</span>}
      {runtime && <span className="type-data">{runtime}</span>}
      {seasons && <span className="type-data">{seasons}</span>}
      {card.artist && <span className="truncate">{card.artist}</span>}
      {certification && (
        <span
          title={t('discover.card.certification', { rating: certification })}
          className="type-data inline-flex h-5 items-center rounded-control border-[1.5px] border-current px-1.5 text-xs font-semibold text-text"
        >
          <span className="sr-only">
            {t('discover.card.certification', { rating: certification })}
          </span>
          <span aria-hidden>{certification}</span>
        </span>
      )}
      {genres && card.genres.length > 0 && <span>{card.genres.join(', ')}</span>}
    </p>
  )
}
