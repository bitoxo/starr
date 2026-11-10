import type { ComponentProps } from 'react'

import { cn } from '@/lib/utils'

/*
 * The building blocks of uniform request rows (spec 0008, amendment "uniform rows"): the member
 * list and the admin table share one thumb slot and one state cell, so titles start on the same
 * x and every row has the same height whatever the cover's shape.
 */

const SLOT = {
  /** Member rows: 48 × 72. */
  row: 'h-18 w-12',
  /** Dense admin tables: 32 × 48. */
  table: 'h-12 w-8',
} as const

/**
 * A fixed slot for a row's cover: a 2:3 poster fills it, a square cover sits centred in it
 * (the cover takes the slot's width).
 */
export function ThumbSlot({
  size,
  className,
  ...props
}: ComponentProps<'div'> & { size: keyof typeof SLOT }) {
  return (
    <div
      data-thumb
      {...props}
      className={cn('flex shrink-0 items-center', SLOT[size], className)}
    />
  )
}

/** A row's state: the chip on top, one detail line below where there is one. */
export function StateCell({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      data-state-cell
      {...props}
      className={cn('flex min-w-0 flex-col items-start gap-1', className)}
    />
  )
}
