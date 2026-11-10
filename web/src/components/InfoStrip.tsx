import { PlayIcon } from 'lucide-react'
import { useRef } from 'react'
import { useTranslation } from 'react-i18next'

import { categoryOfKind, coverImage, isMusic, tmdbImage } from '@/api/categories'
import { type TitleCard, useOpenIn } from '@/api/discover'
import { CoverFallback } from '@/components/CoverFallback'
import { QuickRequest } from '@/components/QuickRequest'
import { TitleFacts } from '@/components/TitleFacts'
import { Button } from '@/components/ui/button'
import { cardState } from '@/lib/states'

/**
 * Info strip (spec 0008 A3, Q3 C; keyboard and touch): under a lane, the lane's current cover
 * with its image, title, facts (artist for albums), a two-line synopsis and the same two
 * actions as the preview card, which follow the lane in the tab order: an available title opens
 * in its media server (gold, the strip's one action), else Request or the state chip; then
 * Details. `onOpen` opens the detail view from the lane's cover, so focus comes back to the
 * cover when it closes.
 */
export function InfoStrip({
  card,
  requestable,
  onOpen,
}: {
  card: TitleCard
  requestable: boolean
  onOpen: () => void
}) {
  const { t } = useTranslation('member')
  const details = useRef<HTMLButtonElement>(null)
  const state = cardState(card)
  const openIn = useOpenIn(card, state?.key === 'available' || state?.key === 'partlyAvailable')
  const image = card.backdrop
    ? tmdbImage(card.backdrop, 'w300')
    : card.poster
      ? isMusic(card.kind)
        ? coverImage(card.poster, '250')
        : tmdbImage(card.poster, 'w185')
      : null
  return (
    <div
      role="group"
      aria-label={t('discover.card.about', { title: card.title })}
      data-info-strip
      className="grid items-center gap-x-5 gap-y-3 rounded-card border border-line bg-night-1 p-3 sm:grid-cols-[9rem_minmax(0,1fr)_auto] motion-safe:animate-in motion-safe:fade-in-0 motion-safe:duration-(--dur-2)"
    >
      <div className="max-sm:hidden">
        {image ? (
          <img
            src={image}
            alt=""
            decoding="async"
            className="aspect-video w-full rounded-control bg-night-2 object-cover"
          />
        ) : (
          <CoverFallback
            title={card.title}
            category={categoryOfKind(card.kind)}
            compact
            className="aspect-video rounded-control"
          />
        )}
      </div>
      <div className="flex min-w-0 flex-col gap-1">
        <h3 title={card.title} className="type-display truncate text-lg font-bold text-text">
          {card.title}
        </h3>
        <TitleFacts card={card} />
        {card.overview && <p className="line-clamp-2 text-sm text-text-2">{card.overview}</p>}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <QuickRequest
          card={card}
          requestable={requestable}
          onOpen={onOpen}
          size="sm"
          className="max-lg:h-11 max-lg:px-5"
          focusAfter={details}
        />
        {openIn.link && (
          <Button asChild variant="primary" size="sm" className="max-lg:h-11 max-lg:px-5">
            <a href={openIn.link.url} target="_blank" rel="noreferrer">
              <PlayIcon aria-hidden className="fill-current" />
              {t('title.openIn', { service: openIn.link.name })}
            </a>
          </Button>
        )}
        <Button
          ref={details}
          size="sm"
          className="max-lg:h-11 max-lg:px-5"
          aria-label={t('discover.card.detailsOf', { title: card.title })}
          onClick={onOpen}
        >
          {t('discover.card.details')}
        </Button>
      </div>
    </div>
  )
}
