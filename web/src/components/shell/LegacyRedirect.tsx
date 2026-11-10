import type { ReactNode } from 'react'
import { Navigate, useLocation } from 'react-router'

import { legacyAdminTarget } from './legacyRoutes'

/**
 * An old admin URL (spec 0008 Q22): rendered once, then replaced by its new place, so the old
 * entry leaves the history whether it came from a bookmark, an email or a link (Back never
 * loops). `children` render for a current URL that shares the path (`/admin/requests`).
 */
export function LegacyRedirect({ children = null }: { children?: ReactNode }) {
  const { pathname, search, hash } = useLocation()
  const target = legacyAdminTarget(pathname, search)
  if (target === null) return children
  return <Navigate to={target + hash} replace />
}
