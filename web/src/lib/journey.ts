import { useTranslation } from 'react-i18next'

import type { MediaKind } from '@/api/discover'

/** "Verity · Movie", "Dark · Season 2", "Radiohead · OK Computer": the card's heading. */
export function useJourneyTitle() {
  const { t } = useTranslation()
  return (title: string, kind: MediaKind, season?: number | null, album?: string | null) =>
    [
      title,
      album ?? (season != null ? t('journey.season', { number: season }) : t(`journey.${kind}`)),
    ].join(' · ')
}

/** Album titles by release-group id, for the journey card of a music request. */
export function albumTitles(items: { album?: { id: string; title: string } | null }[]) {
  return Object.fromEntries(items.flatMap((i) => (i.album ? [[i.album.id, i.album.title]] : [])))
}
