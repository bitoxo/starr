import { useSyncExternalStore } from 'react'

/*
 * Motion (design README A2, spec 0008 A2): CSS motion goes through Tailwind's `motion-safe:`
 * variants and the `--dur-*`/`--ease-*` tokens in `index.css`; script-driven motion (smooth
 * scrolling, timers) asks `useReducedMotion()`. With `prefers-reduced-motion: reduce` nothing
 * translates or scales.
 */

const QUERY = '(prefers-reduced-motion: reduce)'

function media(): MediaQueryList | null {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia(QUERY)
    : null
}

/** The user asked for reduced motion (now, outside React). */
export function prefersReducedMotion(): boolean {
  return media()?.matches ?? false
}

function subscribe(onChange: () => void): () => void {
  const list = media()
  list?.addEventListener('change', onChange)
  return () => list?.removeEventListener('change', onChange)
}

/** The user asked for reduced motion; follows changes of the setting. */
export function useReducedMotion(): boolean {
  return useSyncExternalStore(subscribe, prefersReducedMotion, () => false)
}

/** `scroll-behavior` for script-driven scrolling: smooth only with motion allowed. */
export function scrollBehavior(): ScrollBehavior {
  return prefersReducedMotion() ? 'auto' : 'smooth'
}

/**
 * Cover lift on hover and keyboard focus (lift 3 px, scale 1.03, `--dur-2`), only with motion
 * allowed; the Polaris ring is applied separately and stays under reduced motion. `trigger` is
 * the variant that marks focus: the group itself, or a `.peer` control inside it.
 */
export function coverLift(trigger: 'group' | 'peer' = 'group'): string {
  const focus =
    trigger === 'peer'
      ? 'motion-safe:group-has-[.peer:focus-visible]:-translate-y-[3px] motion-safe:group-has-[.peer:focus-visible]:scale-[1.03]'
      : 'motion-safe:group-focus-visible:-translate-y-[3px] motion-safe:group-focus-visible:scale-[1.03]'
  return `motion-safe:transition-transform motion-safe:duration-(--dur-2) motion-safe:ease-out motion-safe:group-hover:-translate-y-[3px] motion-safe:group-hover:scale-[1.03] ${focus}`
}
