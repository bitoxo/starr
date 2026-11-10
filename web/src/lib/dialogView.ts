import { type RefObject, useEffect } from 'react'

/**
 * A notice that appears inside a scrolled dialog body (e.g. a submit error at its top) is
 * brought into view, so sighted users see the reaction too.
 */
export function useScrollIntoDialogView(ref: RefObject<HTMLElement | null>) {
  useEffect(() => {
    const el = ref.current
    if (el?.closest('[data-slot="dialog-body"]')) el.scrollIntoView?.({ block: 'nearest' })
  }, [ref])
}
