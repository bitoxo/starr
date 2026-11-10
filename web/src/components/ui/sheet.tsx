import { XIcon } from 'lucide-react'
import { Dialog as SheetPrimitive } from 'radix-ui'
import type { ComponentProps } from 'react'
import { useTranslation } from 'react-i18next'

import { useReturnFocus } from '@/lib/returnFocus'
import { cn } from '@/lib/utils'

// Side drawer built on the dialog primitive (used for the mobile navigation).
export function Sheet(props: ComponentProps<typeof SheetPrimitive.Root>) {
  return <SheetPrimitive.Root data-slot="sheet" {...props} />
}

export function SheetTrigger(props: ComponentProps<typeof SheetPrimitive.Trigger>) {
  return <SheetPrimitive.Trigger data-slot="sheet-trigger" {...props} />
}

export function SheetContent({
  className,
  children,
  side = 'left',
  onOpenAutoFocus,
  onCloseAutoFocus,
  ...props
}: ComponentProps<typeof SheetPrimitive.Content> & { side?: 'left' | 'right' }) {
  const { t } = useTranslation()
  // Focus back to the opener on close (spec 0008 F3).
  const focus = useReturnFocus()
  return (
    <SheetPrimitive.Portal>
      <SheetPrimitive.Overlay
        data-slot="sheet-overlay"
        className="fixed inset-0 z-50 bg-night-0/80 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:animate-in data-[state=open]:fade-in-0"
      />
      <SheetPrimitive.Content
        data-slot="sheet-content"
        className={cn(
          'fixed inset-y-0 z-50 flex h-full w-72 max-w-[85vw] flex-col border-line bg-night-1',
          'data-[state=open]:animate-in data-[state=closed]:animate-out motion-reduce:animate-none',
          side === 'left'
            ? 'left-0 border-r data-[state=closed]:slide-out-to-left data-[state=open]:slide-in-from-left'
            : 'right-0 border-l data-[state=closed]:slide-out-to-right data-[state=open]:slide-in-from-right',
          className,
        )}
        {...props}
        onOpenAutoFocus={(event) => {
          focus.capture()
          onOpenAutoFocus?.(event)
        }}
        onCloseAutoFocus={(event) => {
          onCloseAutoFocus?.(event)
          if (!event.defaultPrevented) focus.onCloseAutoFocus(event)
        }}
      >
        {children}
        <SheetPrimitive.Close className="absolute top-4 right-3 cursor-pointer rounded-full p-2 text-text-3 transition-colors hover:bg-night-2 hover:text-text [&_svg]:size-4">
          <XIcon aria-hidden />
          <span className="sr-only">{t('actions.close')}</span>
        </SheetPrimitive.Close>
      </SheetPrimitive.Content>
    </SheetPrimitive.Portal>
  )
}

export function SheetTitle({ className, ...props }: ComponentProps<typeof SheetPrimitive.Title>) {
  return <SheetPrimitive.Title data-slot="sheet-title" className={cn(className)} {...props} />
}

export function SheetDescription({
  className,
  ...props
}: ComponentProps<typeof SheetPrimitive.Description>) {
  return (
    <SheetPrimitive.Description
      data-slot="sheet-description"
      className={cn(className)}
      {...props}
    />
  )
}
