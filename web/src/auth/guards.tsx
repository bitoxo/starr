import { Navigate, Outlet, useLocation } from 'react-router'

import { afterSignIn, loginPath } from '@/auth/next'
import { homePath, SECURITY_PATH, useNeedsSetup, useSession, type Role } from '@/auth/session'
import { ShellSkeleton } from '@/components/shell/ShellSkeleton'

/**
 * Renders the child routes only for `role`; everyone else goes to their own home.
 * Admins who must enroll TOTP first (`STARR_ADMIN_TOTP_REQUIRED`) only see the
 * security page: the server refuses every other admin API until then. While the session is still
 * loading, the shell's skeleton stands in, so the page never starts blank (spec 0008 MEM-13).
 */
export function RequireRole({ role }: { role: Exclude<Role, 'none'> }) {
  const session = useSession()
  const location = useLocation()
  if (session.pending) return <ShellSkeleton variant={role} />
  // Signed out: to the login page, which brings the visitor back here (AUTH-02).
  if (session.role === 'none') return <Navigate to={loginPath(location)} replace />
  if (session.role !== role) return <Navigate to={homePath(session.role)} replace />
  if (session.me?.totpEnrollmentRequired && location.pathname !== SECURITY_PATH) {
    return <Navigate to={SECURITY_PATH} replace />
  }
  return <Outlet />
}

const SETUP_PATH = '/setup'

/**
 * Signed-in accounts don't see login/setup again: they go on to a valid `?next=` or home, and
 * after first-run setup to the Inbox's setup checklist (spec 0008 slice 11). Signed-out visitors
 * land on setup while no admin exists (first run) and on login afterwards.
 */
export function GuestOnly() {
  const session = useSession()
  const needsSetup = useNeedsSetup()
  const location = useLocation()
  if (session.pending) return null
  if (session.role !== 'none') {
    return <Navigate to={afterSignIn(session.role, location.pathname, location.search)} replace />
  }
  if (needsSetup === undefined) return null
  const onSetup = location.pathname === SETUP_PATH
  if (needsSetup && !onSetup) return <Navigate to={SETUP_PATH} replace />
  if (!needsSetup && onSetup) return <Navigate to={homePath('none')} replace />
  return <Outlet />
}
