import { cn } from '@/lib/utils'

/**
 * A placeholder in the final shape while data loads (spec 0008 F10): a `night-2` block that
 * pulses only with motion allowed. Decorative; the region around it says `aria-busy`.
 */
export function Skeleton({ className }: { className?: string }) {
  return (
    <span
      aria-hidden
      data-skeleton
      className={cn('block rounded-control bg-night-2 motion-safe:animate-pulse', className)}
    />
  )
}
