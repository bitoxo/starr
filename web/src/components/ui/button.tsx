import { cva, type VariantProps } from 'class-variance-authority'
import { Slot } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '@/lib/utils'

// Pill buttons. `primary` (Polaris) is reserved for the one primary action per view.
const buttonVariants = cva(
  'inline-flex shrink-0 cursor-pointer items-center justify-center gap-2 rounded-full font-medium whitespace-nowrap transition-colors duration-150 disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        primary: 'bg-polaris text-polaris-ink hover:bg-polaris/90',
        secondary: 'border border-line bg-night-2 text-text hover:border-text-3',
        ghost: 'text-text-2 hover:bg-night-2 hover:text-text',
      },
      // Below 1024 px (touch layouts) every size reaches the 44 px target (design README).
      size: {
        default: 'h-10 px-5 text-base max-lg:h-11',
        sm: 'h-8 px-3.5 text-sm max-lg:h-11',
        icon: 'size-10 max-lg:size-11',
      },
    },
    defaultVariants: { variant: 'secondary', size: 'default' },
  },
)

export function Button({
  className,
  variant,
  size,
  asChild = false,
  ...props
}: ComponentProps<'button'> & VariantProps<typeof buttonVariants> & { asChild?: boolean }) {
  const Comp = asChild ? Slot.Root : 'button'
  return (
    <Comp
      data-slot="button"
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  )
}
