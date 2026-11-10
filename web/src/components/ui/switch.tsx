import { Switch as SwitchPrimitive } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '@/lib/utils'

// Neutral when off, starlight when on: Polaris stays reserved for the primary action.
export function Switch({ className, ...props }: ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      data-slot="switch"
      className={cn(
        // The hit area reaches 44 px on touch layouts; the track stays small (Starr's touch rule).
        'peer relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border border-line bg-night-0 transition-colors max-lg:after:absolute max-lg:after:-inset-x-1 max-lg:after:-inset-y-3',
        'data-[state=checked]:border-text-2 data-[state=checked]:bg-text-2',
        'disabled:cursor-not-allowed disabled:opacity-50',
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb
        className={cn(
          'pointer-events-none block size-3.5 translate-x-0.5 rounded-full bg-text-3 transition-transform motion-reduce:transition-none',
          'data-[state=checked]:translate-x-4.5 data-[state=checked]:bg-night-0',
        )}
      />
    </SwitchPrimitive.Root>
  )
}
