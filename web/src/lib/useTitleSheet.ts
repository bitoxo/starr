import { useLocation, useNavigate, useSearchParams } from 'react-router'

import type { TitleRef } from '@/api/discover'
import { prefersReducedMotion } from '@/lib/motion'
import { parseTitleRef, titleRefParam } from '@/lib/seasons'

/** The view-transition name the opening poster and the detail cover share (spec 0008 A7). */
export const COVER_MORPH = 'detail-cover'

/**
 * Lends the opening poster's image the shared name, so the browser morphs it into the detail
 * cover (View Transitions, only with motion allowed). The detail cover takes the name back from
 * it while the new view renders (`takeCoverMorph`): two elements may not share it.
 */
function lendCoverMorph(): boolean {
  if (typeof document.startViewTransition !== 'function' || prefersReducedMotion()) return false
  const opener = document.activeElement
  const image = opener instanceof HTMLElement ? opener.querySelector('img') : null
  if (!image) return false
  image.style.setProperty('view-transition-name', COVER_MORPH)
  image.dataset.coverMorph = ''
  // Nothing took it (no cover rendered): give it up, so the next open has no duplicate name.
  window.setTimeout(() => {
    if (image.dataset.coverMorph === undefined) return
    image.style.removeProperty('view-transition-name')
    delete image.dataset.coverMorph
  }, 1000)
  return true
}

/** Takes the shared name from the poster that lent it; true when a morph is under way. */
export function takeCoverMorph(): boolean {
  const lent = document.querySelectorAll<HTMLElement>('[data-cover-morph]')
  lent.forEach((el) => {
    el.style.removeProperty('view-transition-name')
    delete el.dataset.coverMorph
  })
  return lent.length > 0
}

/**
 * The open detail view lives in `?title=movie-27205`, so it is linkable and Back closes it.
 * `raw` is set whenever the parameter is there, also when it names nothing valid: the overlay
 * then says "not found" (MEM-32).
 */
export function useTitleSheet() {
  const [params, setParams] = useSearchParams()
  const location = useLocation()
  const navigate = useNavigate()
  const raw = params.get('title')
  const ref = parseTitleRef(raw)
  // How many entries the overlay pushed (artist → album → …): closing goes back past all of
  // them, so Back afterwards does not reopen it. A deep link has none.
  const depth = overlayDepth(location.state)
  const open = (next: TitleRef) =>
    setParams(
      (p) => {
        p.set('title', titleRefParam({ kind: next.kind, id: next.id }))
        return p
      },
      { viewTransition: lendCoverMorph(), state: { [DEPTH]: depth + 1 } },
    )
  const close = () => {
    if (depth > 0) {
      navigate(-depth)
      return
    }
    // A deep link: replace the entry, so Back does not reopen it.
    setParams(
      (p) => {
        p.delete('title')
        return p
      },
      { replace: true },
    )
  }
  return { ref, raw, open, close }
}

const DEPTH = 'detailDepth'

function overlayDepth(state: unknown): number {
  if (state && typeof state === 'object' && DEPTH in state) {
    const value = (state as Record<string, unknown>)[DEPTH]
    if (typeof value === 'number' && Number.isInteger(value) && value > 0) return value
  }
  return 0
}
