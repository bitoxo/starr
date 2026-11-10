import { LockIcon } from 'lucide-react'

import { coverImage, tmdbImage } from '@/api/categories'
import { privateImage } from '@/api/private'
import type { CategoryName } from '@/api/requests'
import { cn } from '@/lib/utils'

/**
 * A small cover for dense admin tables, through Starr's image proxy: 2:3 from TMDB, 1:1 from the
 * Cover Art Archive for music, and 16:9 from the private catalog for a private title the
 * session has unlocked (PRIV-20: never a TMDB URL with a private reference). A neutral private
 * row has no file and shows a lock instead.
 */
export function PosterThumb({
  file,
  title,
  music = false,
  category,
  fill = false,
  className,
}: {
  file?: string | null
  title: string
  music?: boolean
  category?: CategoryName
  /**
   * Takes the width of a `ThumbSlot` (its shape kept, centred in the slot); the lock of a
   * neutral private row fills the whole slot.
   */
  fill?: boolean
  className?: string
}) {
  const adult = category === 'adult'
  const isMusic = music || category === 'music'
  const width = fill ? 'w-full' : adult ? 'w-14' : 'w-9'
  const shape = cn(
    adult ? (fill && !file ? 'h-full' : 'aspect-video') : isMusic ? 'aspect-square' : 'aspect-2/3',
    width,
  )
  if (!file) {
    return (
      <span
        aria-hidden
        title={adult ? undefined : title}
        className={cn(
          shape,
          'type-display grid shrink-0 place-items-center rounded-[6px] bg-night-2 text-sm text-text-3',
          className,
        )}
      >
        {adult ? <LockIcon className="size-4" strokeWidth={1.75} /> : title.slice(0, 1)}
      </span>
    )
  }
  const src = adult
    ? privateImage(file, '300')
    : isMusic
      ? coverImage(file, '250')
      : tmdbImage(file, 'w92')
  return (
    <img
      src={src}
      alt=""
      loading="lazy"
      className={cn(shape, 'shrink-0 rounded-[6px] bg-night-2 object-cover', className)}
    />
  )
}
