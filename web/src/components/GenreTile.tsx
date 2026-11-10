import { type CSSProperties, useEffect, useRef, useState } from 'react'
import { Link } from 'react-router'

import { coverImage, isMusic, tmdbImage } from '@/api/categories'
import { type DiscoverCategory, type DiscoverGenre, useDiscoverList } from '@/api/discover'
import { sampleCoverTint } from '@/lib/tint'
import { cn } from '@/lib/utils'

/** Whether `el` came near the viewport once (always, where nothing can tell). */
function useSeen<T extends HTMLElement>() {
  const ref = useRef<T>(null)
  const [seen, setSeen] = useState(() => typeof IntersectionObserver === 'undefined')
  useEffect(() => {
    const node = ref.current
    if (seen || !node || typeof IntersectionObserver === 'undefined') return
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) setSeen(true)
      },
      { rootMargin: '200px' },
    )
    observer.observe(node)
    return () => observer.disconnect()
  }, [seen])
  return [ref, seen] as const
}

/**
 * A genre tile of a category page (spec 0008 A6, A1): a 16:9 tile in the flat tint of the
 * genre's most popular title, whose cover is cropped at the tile's right edge, and the genre's
 * name in the display face. The cover is asked for only once the tile comes near the viewport
 * (one list request per tile). It links to the genre's "See all" grid. Without a cover, or until
 * its colour is read, the tile stays plain night-1 (never a category colour).
 */
export function GenreTile({
  category,
  genre,
  to,
}: {
  category: DiscoverCategory
  genre: DiscoverGenre
  to: string
}) {
  const [ref, seen] = useSeen<HTMLAnchorElement>()
  const list = useDiscoverList(category.category, 'genre', { genre: genre.id }, { enabled: seen })
  const first = list.data?.pages[0]?.items.find((c) => c.poster)
  const [tint, setTint] = useState<string | null>(null)
  const music = isMusic(category.kind)
  const src = first?.poster
    ? music
      ? coverImage(first.poster, '250')
      : tmdbImage(first.poster, 'w185')
    : null
  return (
    <Link
      ref={ref}
      to={to}
      data-genre-tile
      style={tint ? ({ '--tint': tint } as CSSProperties) : undefined}
      className="tint-surface group relative isolate flex aspect-video flex-col overflow-hidden rounded-card p-4 ring-polaris ring-offset-2 ring-offset-night-0 focus-visible:ring-2 focus-visible:outline-none motion-safe:transition-transform motion-safe:duration-(--dur-2) motion-safe:ease-out motion-safe:hover:-translate-y-[3px] motion-safe:focus-visible:-translate-y-[3px] sm:p-5"
    >
      <span className="type-display relative z-10 max-w-[60%] text-lg leading-tight font-bold text-balance text-text sm:text-xl">
        {genre.name}
      </span>
      {src && (
        <img
          src={src}
          alt=""
          loading="lazy"
          decoding="async"
          onLoad={(event) => setTint(sampleCoverTint(event.currentTarget))}
          className={cn(
            'absolute -right-[6%] -bottom-[18%] w-[34%] rotate-6 rounded-control bg-night-2 object-cover',
            music ? 'aspect-square' : 'aspect-2/3',
          )}
        />
      )}
    </Link>
  )
}
