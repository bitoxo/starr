import type { ReactNode } from 'react'

import { Logo } from '@/components/Logo'
import { cn } from '@/lib/utils'

/**
 * Frame for public pages (setup, login, invitation, set password, forgot password). `art` is a
 * decorative layer (the login poster wall) that takes the right half from 1024 px; the content
 * then keeps to the left half, so no text ever sits on it.
 */
export function AuthLayout({
  eyebrow,
  title,
  description,
  art,
  children,
}: {
  eyebrow?: string
  title: string
  description: string
  art?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="relative flex min-h-dvh flex-col overflow-hidden px-4 py-6 sm:px-8">
      {art}
      <header className="relative">
        <Logo markClassName="size-7" />
      </header>
      <main
        className={cn(
          'relative flex flex-1 items-start justify-center pt-10 pb-16 sm:items-center sm:pt-0',
          art ? 'lg:w-1/2 lg:pr-8' : undefined,
        )}
      >
        <div className="flex w-full max-w-md flex-col gap-8">
          <div className="flex flex-col gap-2">
            {eyebrow && <p className="type-data text-sm text-text-3">{eyebrow}</p>}
            <h1 className="type-display text-3xl font-extrabold text-text">{title}</h1>
            {description && <p className="text-base text-text-2">{description}</p>}
          </div>
          {children}
        </div>
      </main>
    </div>
  )
}
