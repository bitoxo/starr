import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'

import type { StateTone } from '@/lib/states'
import { cn } from '@/lib/utils'

export type StatusTone = StateTone

const TONE: Record<StatusTone, string> = {
  ok: '[--chip:var(--color-ok)]',
  info: '[--chip:var(--color-info)]',
  warn: '[--chip:var(--color-warn)]',
  bad: '[--chip:var(--color-bad)]',
  neutral: '[--chip:var(--color-text-3)]',
}

/**
 * State as form + colour: pill with a leading dot (or icon) in full colour and a text label.
 * Request and journey states use `StateChip`, which takes all three from the state table;
 * this is the primitive under it, for other states (services, runs, accounts).
 */
export function StatusChip({
  tone,
  icon: Icon,
  outline = false,
  children,
  className,
}: {
  tone: StatusTone
  icon?: LucideIcon
  /** A partial state: outlined instead of filled. */
  outline?: boolean
  children: ReactNode
  className?: string
}) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full whitespace-nowrap px-2.5 py-0.5 text-sm font-medium',
        outline
          ? 'border border-(--chip)/60 text-[color-mix(in_oklab,var(--chip)_18%,white)]'
          : 'chip',
        TONE[tone],
        className,
      )}
    >
      {Icon ? (
        <Icon aria-hidden className="size-3.5 shrink-0 text-(--chip)" strokeWidth={2} />
      ) : (
        <span aria-hidden className="size-1.5 rounded-full bg-(--chip)" />
      )}
      {children}
    </span>
  )
}
