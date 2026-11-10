import type { ReactNode } from 'react'

/** The action row: wraps, and fills the card's width on phones (ADM-09). */
export function Actions({ children }: { children: ReactNode }) {
  return (
    <div
      data-actions
      className="flex flex-wrap items-center gap-2 max-md:basis-full max-md:grid max-md:grid-cols-2 max-md:[&>*]:h-11 max-md:[&>*]:w-full max-md:[&>*:only-child]:col-span-2"
    >
      {children}
    </div>
  )
}
