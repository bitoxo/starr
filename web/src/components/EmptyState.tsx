import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'

import { cn } from '@/lib/utils'

/** Designed empty state: icon, title, one-line explanation, optional action. */
export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  children,
  className,
}: {
  icon: LucideIcon
  title: string
  description: string
  action?: ReactNode
  children?: ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        'flex flex-col items-start gap-5 rounded-panel border border-dashed border-line px-6 py-10 sm:px-10 sm:py-14',
        className,
      )}
    >
      <span className="grid size-11 place-items-center rounded-full border border-line bg-night-1 text-text-2">
        <Icon aria-hidden className="size-5" strokeWidth={1.75} />
      </span>
      <div className="flex max-w-lg flex-col gap-1.5">
        <h2 className="type-display text-xl font-bold text-text">{title}</h2>
        <p className="text-base text-text-2">{description}</p>
      </div>
      {children}
      {action}
    </div>
  )
}
