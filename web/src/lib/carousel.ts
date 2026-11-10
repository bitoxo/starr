import { type FocusEvent, type PointerEvent, useEffect, useState } from 'react'

import { isKeyboardFocus } from '@/lib/media'

/*
 * Carousel state (spec 0008 A5, WCAG 2.2.2): which slide shows, and whether it may move on by
 * itself. It auto-advances only while nobody is looking at it closely: it pauses while a mouse
 * hovers it, while keyboard focus is inside, while the tab is hidden, while the page holds it (a detail
 * view is open over it) and after the user pressed pause; under reduced motion it never moves
 * by itself. The dwell itself is the progress bar's CSS animation, whose end calls `advance`, so
 * the bar and the switch can never drift apart. Any move restarts the dwell (`cycle`). The
 * slides' live region is off while it rotates and polite otherwise (WAI-ARIA APG carousel).
 */

export function useCarousel(
  count: number,
  {
    reduced,
    held = false,
  }: {
    /** `prefers-reduced-motion: reduce`: no auto-advance at all. */
    reduced: boolean
    /** Paused from outside, e.g. while a detail view covers the page. */
    held?: boolean
  },
) {
  const [chosen, setChosen] = useState(0)
  const [cycle, setCycle] = useState(0)
  const [userPaused, setUserPaused] = useState(false)
  const [hovered, setHovered] = useState(false)
  const [focused, setFocused] = useState(false)
  const [hidden, setHidden] = useState(() => typeof document !== 'undefined' && document.hidden)

  useEffect(() => {
    const onChange = () => setHidden(document.hidden)
    document.addEventListener('visibilitychange', onChange)
    return () => document.removeEventListener('visibilitychange', onChange)
  }, [])

  const index = count === 0 ? 0 : Math.min(chosen, count - 1)
  const playing = !reduced && count > 1 && !userPaused && !hovered && !focused && !hidden && !held

  /** Show slide `target` (wraps around both ends). */
  const go = (target: number) => {
    if (count === 0) return
    setChosen(((target % count) + count) % count)
    setCycle((c) => c + 1)
  }

  return {
    index,
    /** Changes with every move: the key that restarts the dwell. */
    cycle,
    playing,
    userPaused,
    live: playing ? ('off' as const) : ('polite' as const),
    go,
    /** The dwell ran out: the next slide. */
    advance: () => go(index + 1),
    toggle: () => setUserPaused((p) => !p),
    regionProps: {
      onPointerEnter: (event: PointerEvent) => {
        if (event.pointerType === 'mouse' || event.pointerType === 'pen') setHovered(true)
      },
      onPointerLeave: () => setHovered(false),
      // A click focuses a button too; only keyboard focus holds the carousel, or one click on
      // "Next" would stop it for good once the mouse has left.
      onFocus: (event: FocusEvent) => {
        if (isKeyboardFocus(event.target)) setFocused(true)
      },
      onBlur: (event: FocusEvent) => {
        const next = event.relatedTarget
        if (!(next instanceof Node) || !event.currentTarget.contains(next)) setFocused(false)
      },
    },
  }
}
