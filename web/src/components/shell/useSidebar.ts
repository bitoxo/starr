import { useCallback, useState, useSyncExternalStore } from 'react'
import { useLocation } from 'react-router'

import { readSidebarMode, writeSidebarMode, type SidebarMode } from './prefs'

const hasMatchMedia = () => typeof window !== 'undefined' && typeof window.matchMedia === 'function'

/** Whether a media query matches, following changes (false where `matchMedia` is missing). */
export function useMediaQuery(query: string): boolean {
  return useSyncExternalStore(
    (onChange) => {
      if (!hasMatchMedia()) return () => {}
      const list = window.matchMedia(query)
      list.addEventListener('change', onChange)
      return () => list.removeEventListener('change', onChange)
    },
    () => hasMatchMedia() && window.matchMedia(query).matches,
    () => false,
  )
}

export const DESKTOP = '(min-width: 1024px)'
export const WIDE = '(min-width: 1280px)'

export interface SidebarState {
  /** ≥ 1024 px: a sidebar exists at all (below: top bar and tab bar). */
  desktop: boolean
  /** Labels shown (expanded) rather than icons only (rail). */
  expanded: boolean
  /** 1024–1279 px and expanded: the sidebar lies over the content without reflowing it. */
  overlay: boolean
  toggle: () => void
  close: () => void
}

/**
 * Sidebar behaviour (spec 0008 W1, Q10): ≥ 1280 px expanded by default, the toggle collapses to
 * a rail and the choice is remembered per browser; 1024–1279 px a rail whose toggle expands it
 * over the content for the moment (closed again on navigation); below, no sidebar.
 */
export function useSidebar(): SidebarState {
  const desktop = useMediaQuery(DESKTOP)
  const wide = useMediaQuery(WIDE)
  const [mode, setMode] = useState<SidebarMode | null>(readSidebarMode)
  const { pathname } = useLocation()
  // The overlay is for the moment: it belongs to the page it was opened on, so a new page
  // closes it, and it only exists between 1024 and 1279 px.
  const [overlayOn, setOverlayOn] = useState<string | null>(null)
  const overlayOpen = desktop && !wide && overlayOn === pathname

  const expanded = wide ? mode !== 'rail' : overlayOpen
  const toggle = useCallback(() => {
    if (wide) {
      const next: SidebarMode = expanded ? 'rail' : 'expanded'
      setMode(next)
      writeSidebarMode(next)
    } else {
      setOverlayOn(overlayOpen ? null : pathname)
    }
  }, [wide, expanded, overlayOpen, pathname])
  const close = useCallback(() => setOverlayOn(null), [])

  return { desktop, expanded, overlay: overlayOpen, toggle, close }
}
