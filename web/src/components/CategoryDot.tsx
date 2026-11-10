import { cn } from '@/lib/utils'

export type Category = 'movies' | 'series' | 'music' | 'adult' | 'podcasts'

const COLOR: Record<Category, string> = {
  movies: 'bg-cat-movies',
  series: 'bg-cat-series',
  music: 'bg-cat-music',
  adult: 'bg-cat-adult',
  podcasts: 'bg-cat-podcasts',
}

/** Small dot in the category colour. Decorative: always pair it with a text label. */
export function CategoryDot({ category, className }: { category: Category; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn('inline-block size-2 shrink-0 rounded-full', COLOR[category], className)}
    />
  )
}
