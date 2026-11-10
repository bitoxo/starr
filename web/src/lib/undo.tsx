import { toast } from 'sonner'

import { UndoToast } from '@/components/UndoToast'

/**
 * Show an undo toast. Private actions pass an id starting with `private:` so the gate can drop
 * it on close (spec 0008 F2), and a neutral message.
 */
export function showUndoToast({
  id,
  message,
  onUndo,
  undoLabel,
}: {
  id?: string
  message: string
  onUndo: () => void
  undoLabel?: string
}) {
  return toast.custom(
    (toastId) => (
      <UndoToast toastId={toastId} message={message} onUndo={onUndo} undoLabel={undoLabel} />
    ),
    // The toast times itself (pausing on hover and focus); sonner must not.
    { id, duration: Number.POSITIVE_INFINITY },
  )
}
