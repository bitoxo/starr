import { useCallback, useLayoutEffect, useRef } from 'react'

/*
 * Focus return (spec 0008 F3): whatever opens a layer (dialog, sheet, popover, overlay) gets
 * focus back when it closes; if the opener is gone (its row was removed, the menu closed) focus
 * goes to the page's first heading. After removing a row focus moves to the next row, else the
 * previous one, else the list heading. Focus never falls to `<body>` (MEM-03).
 */

function focusable(el: HTMLElement | null): el is HTMLElement {
  return (
    el !== null &&
    el.isConnected &&
    !el.closest('[inert]') &&
    !(el instanceof HTMLButtonElement && el.disabled) &&
    el.getAttribute('aria-hidden') !== 'true'
  )
}

/**
 * Focus `el`, making a heading or other static element focusable from code first. `quiet`
 * keeps the view where it is (after a request the list stays put, MEM-10).
 */
export function focusElement(el: HTMLElement, { quiet = false }: { quiet?: boolean } = {}) {
  if (el.tabIndex < 0 && !el.hasAttribute('tabindex')) el.tabIndex = -1
  el.focus({ preventScroll: quiet })
}

/** The page's first heading: the target when nothing better is left. */
export function firstHeading(): HTMLElement | null {
  return document.querySelector<HTMLElement>('main h1') ?? document.querySelector<HTMLElement>('h1')
}

/** Focus `opener` if it is still usable, else `fallback()`, else the first heading. */
export function focusBack(opener: HTMLElement | null, fallback?: () => HTMLElement | null) {
  if (focusable(opener)) {
    opener.focus()
    return
  }
  const target = fallback?.() ?? firstHeading()
  if (target) focusElement(target)
}

/** After removing `items[index]`: the next item, else the previous, else `heading`. */
export function focusAfterRemoval(
  items: HTMLElement[],
  index: number,
  heading?: HTMLElement | null,
) {
  const target = items[index + 1] ?? items[index - 1] ?? heading ?? firstHeading()
  if (target) focusElement(target)
}

const CONTROL = 'a[href], button:not([disabled]), input:not([disabled]), [tabindex="0"]'

/**
 * For a list row the user can remove (hide, withdraw, decide): `arm()` it when the action starts
 * from inside the row; if the row then leaves the list, focus moves to the next row's first
 * control, else the previous one's, else `fallback`'s choice, else the page heading. A row that
 * stays (e.g. "Show hidden" is on) keeps its focus.
 */
export function useFocusNextOnRemoval<T extends HTMLElement>(
  /** Where to go when the row has no neighbour (e.g. the next section of a grouped list). */
  fallback?: (row: T) => HTMLElement | null,
) {
  const node = useRef<T | null>(null)
  const armed = useRef(false)
  const fallbackRef = useRef(fallback)
  useLayoutEffect(() => {
    fallbackRef.current = fallback
  }, [fallback])
  const ref = useCallback((el: T | null) => {
    node.current = el
  }, [])
  useLayoutEffect(() => {
    const row = node.current
    // Runs while the row is still in the DOM, just before React removes it.
    return () => {
      if (!armed.current || !row?.parentElement) return
      // Focus elsewhere (a later switch, another row) stays where the user put it.
      const active = document.activeElement
      if (active && active !== document.body && !row.contains(active)) return
      const rows = Array.from(row.parentElement.children).filter(
        (el): el is HTMLElement => el instanceof HTMLElement,
      )
      const index = rows.indexOf(row)
      const near = [rows[index + 1], rows[index - 1]]
        .map((r) => r?.querySelector<HTMLElement>(CONTROL))
        .find((el): el is HTMLElement => el != null)
      const target = near ?? fallbackRef.current?.(row) ?? firstHeading()
      if (target) focusElement(target)
    }
  }, [])
  return {
    ref,
    arm: () => {
      armed.current = true
    },
    disarm: () => {
      armed.current = false
    },
  }
}

/**
 * Remembers what had focus when a layer opened and gives it back on close. For Radix layers
 * pass `onOpenAutoFocus`/`onCloseAutoFocus`; elsewhere call `capture()` and `restore()`.
 */
export function useReturnFocus() {
  const opener = useRef<HTMLElement | null>(null)

  const capture = useCallback(() => {
    const el = document.activeElement
    opener.current = el instanceof HTMLElement && el !== document.body ? el : null
  }, [])
  /** Focus the opener; if it is gone `fallback()`, else the page heading. */
  const restore = useCallback(
    (fallback?: () => HTMLElement | null) => focusBack(opener.current, fallback),
    [],
  )

  return {
    capture,
    restore,
    /** Radix: runs before focus moves into the layer, so the opener still has it. */
    onOpenAutoFocus: capture,
    /** Radix: replaces its default (the trigger, or `<body>` without one). */
    onCloseAutoFocus: (event: Event) => {
      event.preventDefault()
      restore()
    },
  }
}
