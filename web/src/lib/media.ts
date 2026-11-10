/*
 * Media features for script decisions CSS cannot make alone (spec 0008 A3): whether the device
 * can hover decides if a preview card may appear and if the info strip follows the selection.
 */

function list(query: string): MediaQueryList | null {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia(query)
    : null
}

/** Whether `query` matches now (outside React); false where there is no `matchMedia`. */
export function matchesMedia(query: string): boolean {
  return list(query)?.matches ?? false
}

/** Keyboard focus, as opposed to a click or a tap (which also focus in most browsers). */
export function isKeyboardFocus(el: EventTarget | null): boolean {
  if (!(el instanceof Element)) return false
  try {
    return el.matches(':focus-visible')
  } catch {
    return true
  }
}

/** A touch layout: the primary pointer cannot hover (phones, tablets, TV browsers). */
export const NO_HOVER = '(hover: none)'
