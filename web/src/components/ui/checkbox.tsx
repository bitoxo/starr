import { CheckIcon, MinusIcon } from 'lucide-react'
import { Checkbox as CheckboxPrimitive } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '@/lib/utils'

// Neutral like the switch: checked is starlight, Polaris stays for the primary action.
export function Checkbox({ className, ...props }: ComponentProps<typeof CheckboxPrimitive.Root>) {
  return (
    <CheckboxPrimitive.Root
      data-slot="checkbox"
      className={cn(
        'peer relative grid size-4 shrink-0 cursor-pointer place-items-center rounded-[4px] border border-text-3 bg-night-0 transition-colors max-lg:after:absolute max-lg:after:-inset-3.5',
        'hover:border-text-2 data-[state=checked]:border-text-2 data-[state=checked]:bg-text-2 data-[state=indeterminate]:border-text-2 data-[state=indeterminate]:bg-text-2',
        'disabled:cursor-not-allowed disabled:opacity-50',
        className,
      )}
      {...props}
    >
      <CheckboxPrimitive.Indicator className="text-night-0">
        {props.checked === 'indeterminate' ? (
          <MinusIcon aria-hidden className="size-3" strokeWidth={3} />
        ) : (
          <CheckIcon aria-hidden className="size-3" strokeWidth={3} />
        )}
      </CheckboxPrimitive.Indicator>
    </CheckboxPrimitive.Root>
  )
}
