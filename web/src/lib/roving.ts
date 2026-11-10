import { type KeyboardEvent, useCallback, useRef, useState } from 'react'

import { scrollBehavior } from '@/lib/motion'

/*
 * Roving tabindex (spec 0008 F4, WAI-ARIA APG): a lane or grid is one tab stop. The current item
 * has `tabIndex=0`, all others `-1`; arrow keys move between items, Home/End jump to the ends,
 * Tab leaves the group. In a grid Up/Down move by one row. The moved-to item scrolls into view
 * (smooth only with motion allowed), its container's `scroll-padding` keeps the ring visible.
 */

/** Columns of a wrapping grid, from the items' rendered positions (one row: all of them). */
export function columnsOf(items: (HTMLElement | null)[]): number {
  const present = items.filter((el): el is HTMLElement => el !== null)
  const top = present[0]?.offsetTop
  const firstRow = present.filter((el) => el.offsetTop === top).length
  return Math.max(1, firstRow)
}

export function useRovingFocus<T extends HTMLElement = HTMLElement>(
  count: number,
  { grid = false }: { /** Two dimensions: Up/Down move by a row. */ grid?: boolean } = {},
) {
  const [active, setActive] = useState(0)
  const items = useRef<(T | null)[]>([])
  const current = count === 0 ? 0 : Math.min(active, count - 1)

  const focus = useCallback((index: number) => {
    const el = items.current[index]
    if (!el) return
    setActive(index)
    el.focus({ preventScroll: true })
    el.scrollIntoView?.({ block: 'nearest', inline: 'nearest', behavior: scrollBehavior() })
  }, [])

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
    const from = items.current.findIndex(
      (el) => el !== null && event.target instanceof Node && el.contains(event.target),
    )
    if (from < 0 || from >= count) return
    const row = grid ? columnsOf(items.current.slice(0, count)) : 0
    const to: number | undefined = {
      ArrowRight: from + 1,
      ArrowLeft: from - 1,
      ArrowDown: grid ? from + row : undefined,
      ArrowUp: grid ? from - row : undefined,
      Home: 0,
      End: count - 1,
    }[event.key]
    if (to === undefined) return
    event.preventDefault()
    // Past the ends nothing moves (no wrap, like a list).
    if (to < 0 || to >= count) return
    focus(to)
  }

  /** Props for item `index`: its ref, its place in the tab order, and following focus. */
  const itemProps = (index: number) => ({
    ref: (el: T | null) => {
      items.current[index] = el
    },
    tabIndex: index === current ? 0 : -1,
    onFocus: () => setActive(index),
  })

  return { itemProps, onKeyDown, active: current, focus }
}
