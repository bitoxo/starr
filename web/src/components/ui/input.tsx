import type { ComponentProps } from 'react'

import { cn } from '@/lib/utils'

export function Input({ className, type, ...props }: ComponentProps<'input'>) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        'h-10 w-full min-w-0 rounded-control border border-line bg-night-0 px-3 text-base max-lg:h-11 text-text transition-colors outline-hidden placeholder:text-text-3',
        'hover:border-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-0 focus-visible:outline-polaris/40',
        'disabled:cursor-not-allowed disabled:opacity-50',
        'aria-invalid:border-bad',
        className,
      )}
      {...props}
    />
  )
}
