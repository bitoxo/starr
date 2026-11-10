import { vi } from 'vitest'

/**
 * `window.matchMedia` for `prefers-reduced-motion` (jsdom has none). Returns a switch that
 * flips the setting and tells listeners, like the system setting changing at runtime.
 */
export function mockReducedMotion(reduce: boolean) {
  let matches = reduce
  const listeners = new Set<(event: MediaQueryListEvent) => void>()
  const list = (query: string) =>
    ({
      get matches() {
        return query.includes('prefers-reduced-motion: reduce') ? matches : false
      },
      media: query,
      onchange: null,
      addEventListener: (_: string, fn: (event: MediaQueryListEvent) => void) => listeners.add(fn),
      removeEventListener: (_: string, fn: (event: MediaQueryListEvent) => void) =>
        listeners.delete(fn),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: () => true,
    }) as unknown as MediaQueryList
  vi.stubGlobal('matchMedia', vi.fn(list))
  return {
    set(next: boolean) {
      matches = next
      for (const fn of listeners) fn({ matches: next } as MediaQueryListEvent)
    },
  }
}
