import { useEffect, useRef, type RefObject } from 'react'

/**
 * Calls `onLoad` whenever the returned sentinel element comes near the visible part of `root`
 * (the viewport by default), as long as `enabled`. Endless scrolling for lanes and grids.
 * `loaded` (e.g. the number of pages) re-arms the observer, so a sentinel that is still in
 * view after a page arrived asks for the next one.
 */
export function useLoadMore<T extends HTMLElement>({
  enabled,
  onLoad,
  loaded,
  root,
  margin = '400px',
}: {
  enabled: boolean
  onLoad: () => void
  loaded: number
  root?: RefObject<HTMLElement | null>
  margin?: string
}): RefObject<T | null> {
  const sentinel = useRef<T>(null)
  const load = useRef(onLoad)

  useEffect(() => {
    load.current = onLoad
  }, [onLoad])

  useEffect(() => {
    const node = sentinel.current
    if (!enabled || !node || typeof IntersectionObserver === 'undefined') return
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) load.current()
      },
      { root: root?.current ?? null, rootMargin: margin },
    )
    observer.observe(node)
    return () => observer.disconnect()
  }, [enabled, root, margin, loaded])

  return sentinel
}
