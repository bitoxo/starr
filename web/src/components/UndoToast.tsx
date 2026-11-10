import { Undo2Icon } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

/** How long an undo stays offered (spec 0008 F8). */
const UNDO_MS = 8000

/**
 * The toast after a reversible action (spec 0008 F8): it says what happened and offers Undo for
 * 8 s. The time stops while the pointer is over it or focus is inside, so nobody loses the undo
 * while reaching for it. Undo restores the exact previous state (the caller's `onUndo`).
 */
export function UndoToast({
  toastId,
  message,
  onUndo,
  undoLabel,
  duration = UNDO_MS,
}: {
  toastId: string | number
  message: string
  onUndo: () => void
  undoLabel?: string
  duration?: number
}) {
  const { t } = useTranslation()
  const [hovered, setHovered] = useState(false)
  const [focused, setFocused] = useState(false)
  const remaining = useRef(duration)
  const paused = hovered || focused

  useEffect(() => {
    if (paused) return
    const started = Date.now()
    const timer = window.setTimeout(() => toast.dismiss(toastId), remaining.current)
    return () => {
      window.clearTimeout(timer)
      remaining.current = Math.max(0, remaining.current - (Date.now() - started))
    }
  }, [paused, toastId])

  return (
    <div
      role="status"
      onPointerEnter={() => setHovered(true)}
      onPointerLeave={() => setHovered(false)}
      onFocus={() => setFocused(true)}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) setFocused(false)
      }}
      className="flex w-[min(24rem,calc(100vw-2rem))] items-center gap-3 rounded-card border border-line bg-night-2 px-4 py-3 text-base text-text"
    >
      <span className="min-w-0 flex-1">{message}</span>
      <button
        type="button"
        onClick={() => {
          onUndo()
          toast.dismiss(toastId)
        }}
        className="inline-flex h-9 shrink-0 cursor-pointer items-center gap-1.5 rounded-full border border-line px-3.5 text-sm font-medium text-text hover:border-text-3"
      >
        <Undo2Icon aria-hidden className="size-4" />
        {undoLabel ?? t('actions.undo')}
      </button>
    </div>
  )
}
