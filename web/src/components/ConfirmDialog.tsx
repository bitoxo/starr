import { AlertDialog } from 'radix-ui'
import { type ReactNode, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'

import type { Effect } from '@/api/errors'
import { Button } from '@/components/ui/button'
import { DialogBody } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { useReturnFocus } from '@/lib/returnFocus'
import { cn } from '@/lib/utils'

/*
 * Confirming an action with consequences (spec 0008 F8): the title names the action, the body
 * the consequences the server reported (`effects`), Cancel has the focus and the confirming
 * button is a `bad` outline. Built on Radix AlertDialog: Esc and Cancel close it, focus returns
 * to the opener.
 */

/** One sentence per consequence, in the order the server reported them. */
export function EffectList({ effects, service }: { effects: Effect[]; service: string }) {
  const { t } = useTranslation(['admin', 'common'])
  const category = (c: string) => t(`common:categories.${c}` as 'common:categories.movies')
  return (
    <ul className="flex list-disc flex-col gap-2 pl-5 text-base text-text-2 marker:text-text-3">
      {effects.map((effect) => (
        <li key={effect.kind + ('category' in effect ? effect.category : '')}>
          {effect.kind === 'openRequestsFinish'
            ? t('admin:effects.openRequestsFinish', { count: effect.count, service })
            : t(`admin:effects.${effect.kind}`, { category: category(effect.category), service })}
        </li>
      ))}
    </ul>
  )
}

export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  children,
  confirmLabel,
  confirmPhrase,
  pending = false,
  onConfirm,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: string
  children?: ReactNode
  confirmLabel: string
  /**
   * Type to confirm: the name to type (a service or member in use) before the destructive
   * button works.
   */
  confirmPhrase?: string
  pending?: boolean
  onConfirm: () => void
}) {
  const { t } = useTranslation()
  // Opened from code, not a Radix trigger: remember the opener to give focus back (F3).
  const focus = useReturnFocus()
  const phraseId = useId()
  const [typed, setTyped] = useState('')
  const confirmed = !confirmPhrase || typed.trim() === confirmPhrase
  // A new opening starts empty again, however it was closed.
  const [wasOpen, setWasOpen] = useState(open)
  if (open !== wasOpen) {
    setWasOpen(open)
    if (!open) setTyped('')
  }
  return (
    <AlertDialog.Root open={open} onOpenChange={onOpenChange}>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="fixed inset-0 z-50 bg-night-0/80 data-[state=open]:animate-in data-[state=open]:fade-in-0" />
        <AlertDialog.Content
          onOpenAutoFocus={focus.onOpenAutoFocus}
          onCloseAutoFocus={focus.onCloseAutoFocus}
          className={cn(
            // Bounded like `DialogContent`: title and buttons stay, the middle scrolls.
            'fixed top-1/2 left-1/2 z-50 flex max-h-[calc(100dvh-48px)] w-[calc(100%-2rem)] max-w-lg -translate-x-1/2 -translate-y-1/2 flex-col gap-5 overflow-y-auto rounded-panel border border-line bg-night-2 p-6',
            'data-[state=open]:animate-in data-[state=open]:fade-in-0 motion-safe:data-[state=open]:zoom-in-95',
          )}
        >
          <div className="flex shrink-0 flex-col gap-1.5">
            <AlertDialog.Title className="type-display text-xl font-bold text-text">
              {title}
            </AlertDialog.Title>
            {description ? (
              <AlertDialog.Description className="text-base text-text-2">
                {description}
              </AlertDialog.Description>
            ) : null}
          </div>
          {(children || confirmPhrase) && (
            <DialogBody>
              {children}
              {confirmPhrase && (
                <div className="flex flex-col gap-1.5">
                  <label htmlFor={phraseId} className="text-sm font-medium text-text-2">
                    {t('confirm.typeToConfirm', { phrase: confirmPhrase })}
                  </label>
                  <Input
                    id={phraseId}
                    value={typed}
                    autoComplete="off"
                    spellCheck={false}
                    onChange={(e) => setTyped(e.target.value)}
                  />
                </div>
              )}
            </DialogBody>
          )}
          <div className="flex shrink-0 flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <AlertDialog.Cancel asChild>
              <Button variant="ghost" className="max-sm:h-11">
                {t('actions.cancel')}
              </Button>
            </AlertDialog.Cancel>
            <Button
              className="border-bad/60 text-bad hover:border-bad max-sm:h-11"
              disabled={pending || !confirmed}
              onClick={onConfirm}
            >
              {confirmLabel}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  )
}
