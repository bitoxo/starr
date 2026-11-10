import { useId } from 'react'
import { useTranslation } from 'react-i18next'

import type { AlbumDetails } from '@/api/discover'
import { discs } from '@/lib/detail'
import { formatNumber, formatSeconds, formatTrackLength } from '@/lib/format'

/**
 * An album's tracklist (spec 0008 A7): disc headings when there are several, position, title,
 * the track's artist where it differs, length in Geist Mono, and the total running time. It is
 * the tracklist of the release the track count is of; with several releases the view says which.
 */
export function Tracklist({ album }: { album: AlbumDetails }) {
  const { t } = useTranslation('member')
  const { discs: list, totalSeconds } = discs(album.tracks)
  const release = album.tracklistRelease
  const headingId = useId()
  if (album.tracks.length === 0) {
    return <p className="text-base text-text-3">{t('detail.tracks.empty')}</p>
  }
  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-4">
      <div className="flex flex-col gap-0.5">
        <h3 id={headingId} className="type-display text-xl font-bold text-text">
          {t('detail.tracks.heading')}
        </h3>
        {release && release.releaseCount > 1 && release.date && (
          <p className="text-sm text-text-3">
            {t('detail.tracks.release', { year: release.date.slice(0, 4) })}
          </p>
        )}
      </div>
      {list.map((disc) => (
        <div key={disc.disc} className="flex flex-col">
          {list.length > 1 && (
            <h4 className="type-data mb-1 border-b border-line px-3 pb-2 text-xs tracking-wide text-text-3 uppercase">
              {disc.title
                ? t('detail.tracks.discTitled', { number: disc.disc, title: disc.title })
                : t('detail.tracks.disc', { number: disc.disc })}
            </h4>
          )}
          <ol className="flex flex-col">
            {disc.tracks.map((track) => (
              <li
                key={`${track.disc}-${track.position}`}
                className="grid grid-cols-[2rem_minmax(0,1fr)_auto] items-baseline gap-3 rounded-control px-3 py-2.5 text-base transition-colors duration-(--dur-1) hover:bg-text/[0.04]"
              >
                <span className="type-data text-right text-sm text-text-3">
                  {formatNumber(track.position)}
                </span>
                <span className="min-w-0 text-text">
                  {track.title}
                  {track.artist && <span className="ml-2 text-sm text-text-3">{track.artist}</span>}
                </span>
                <span className="type-data text-right text-sm text-text-2">
                  {track.lengthSeconds != null && formatTrackLength(track.lengthSeconds)}
                </span>
              </li>
            ))}
          </ol>
        </div>
      ))}
      <p className="flex justify-between gap-4 border-t border-line px-3 pt-3 text-sm text-text-3">
        <span>{t('music.tracks', { count: album.tracks.length })}</span>
        {totalSeconds != null && <span className="type-data">{formatSeconds(totalSeconds)}</span>}
      </p>
    </section>
  )
}
