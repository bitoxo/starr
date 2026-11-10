import { type Category, CategoryDot } from '@/components/CategoryDot'
import { cn } from '@/lib/utils'

/**
 * The one stand-in for a missing or broken image, in every shape (spec 0008 A3, MEM-25):
 * `night-2` tile, the category dot, the title in the display face, line-clamped, and the full
 * title as a tooltip (MEM-26). `compact` for small tiles (thumbnails, 16:9 scenes).
 */
export function CoverFallback({
  title,
  category,
  compact = false,
  className,
}: {
  title: string
  category: Category
  compact?: boolean
  className?: string
}) {
  return (
    <span
      aria-hidden
      title={title}
      data-cover-fallback
      className={cn(
        'type-display flex w-full flex-col items-center justify-center gap-2 overflow-hidden border border-line bg-night-2 text-center font-bold text-text-2',
        compact ? 'p-2 text-sm leading-tight' : 'p-3 text-lg leading-snug',
        className,
      )}
    >
      <CategoryDot category={category} className={compact ? 'size-1.5' : 'size-2'} />
      <span className={cn('min-w-0 break-words', compact ? 'line-clamp-2' : 'line-clamp-3')}>
        {title}
      </span>
    </span>
  )
}
