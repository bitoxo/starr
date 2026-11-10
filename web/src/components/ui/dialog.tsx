import { XIcon } from 'lucide-react'
import { Dialog as DialogPrimitive } from 'radix-ui'
import type { ComponentProps } from 'react'
import { useTranslation } from 'react-i18next'

import { useReturnFocus } from '@/lib/returnFocus'
import { cn } from '@/lib/utils'

export function Dialog(props: ComponentProps<typeof DialogPrimitive.Root>) {
  return <DialogPrimitive.Root data-slot="dialog" {...props} />
}

export function DialogTrigger(props: ComponentProps<typeof DialogPrimitive.Trigger>) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />
}

export function DialogClose(props: ComponentProps<typeof DialogPrimitive.Close>) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />
}

export function DialogOverlay({
  className,
  ...props
}: ComponentProps<typeof DialogPrimitive.Overlay>) {
  return (
    <DialogPrimitive.Overlay
      data-slot="dialog-overlay"
      className={cn(
        'fixed inset-0 z-50 bg-night-0/80',
        'data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=closed]:animate-out data-[state=closed]:fade-out-0',
        className,
      )}
      {...props}
    />
  )
}

/**
 * A modal dialog. Focus goes back to whatever opened it, or to the page heading if that is gone
 * (spec 0008 F3, MEM-03); Esc closes it (only the top layer, ADM-37).
 *
 * Always bounded by the viewport (24 px margin top and bottom): header and footer stay in place
 * and `DialogBody` scrolls; a form wrapping all three passes the bounds through. Content without
 * a `DialogBody` scrolls as a whole, so nothing is ever out of reach.
 */
export function DialogContent({
  className,
  children,
  onOpenAutoFocus,
  onCloseAutoFocus,
  ...props
}: ComponentProps<typeof DialogPrimitive.Content>) {
  const { t } = useTranslation()
  const focus = useReturnFocus()
  return (
    <DialogPrimitive.Portal>
      <DialogOverlay />
      <DialogPrimitive.Content
        data-slot="dialog-content"
        className={cn(
          'fixed top-1/2 left-1/2 z-50 flex max-h-[calc(100dvh-48px)] w-[calc(100%-2rem)] max-w-md -translate-x-1/2 -translate-y-1/2 flex-col gap-5 overflow-y-auto rounded-panel border border-line bg-night-2 p-6',
          '[&>form]:flex [&>form]:min-h-0 [&>form]:flex-col',
          'data-[state=open]:animate-in data-[state=open]:fade-in-0 motion-safe:data-[state=open]:zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 motion-safe:data-[state=closed]:zoom-out-95',
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
        <DialogPrimitive.Close className="absolute top-4 right-4 cursor-pointer rounded-full p-1.5 text-text-3 transition-colors hover:bg-night-1 hover:text-text [&_svg]:size-4">
          <XIcon aria-hidden />
          <span className="sr-only">{t('actions.close')}</span>
        </DialogPrimitive.Close>
      </DialogPrimitive.Content>
    </DialogPrimitive.Portal>
  )
}

export function DialogHeader({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      data-slot="dialog-header"
      className={cn('flex shrink-0 flex-col gap-1.5 pr-8', className)}
      {...props}
    />
  )
}

/** The part of a dialog between header and footer; it scrolls when the dialog is too tall. */
export function DialogBody({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      data-slot="dialog-body"
      // Bleeds to the dialog's edges so focus rings, the scrollbar and the 44 px hit areas of
      // checkboxes below 1024 px are neither clipped nor make the body scroll for nothing.
      className={cn(
        '-mx-6 -mt-1 -mb-3.5 flex min-h-0 flex-col gap-5 overflow-y-auto px-6 pt-1 pb-3.5',
        className,
      )}
      {...props}
    />
  )
}

export function DialogFooter({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn('flex shrink-0 flex-col-reverse gap-2 sm:flex-row sm:justify-end', className)}
      {...props}
    />
  )
}

export function DialogTitle({ className, ...props }: ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn('type-display text-xl font-bold', className)}
      {...props}
    />
  )
}

export function DialogDescription({
  className,
  ...props
}: ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn('text-base text-text-2', className)}
      {...props}
    />
  )
}
