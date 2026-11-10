/** Old admin path → its place in the grouped admin area (spec 0008 Q22, ADR 0015). */
const MOVED: Record<string, string> = {
  '/admin/needs-you': '/admin/inbox',
  '/admin/members': '/admin/people/members',
  '/admin/invitations': '/admin/people/invitations',
  '/admin/settings/admins': '/admin/people/admins',
  '/admin/services': '/admin/media/services',
  '/admin/accounts': '/admin/media/accounts',
  '/admin/categories': '/admin/media/categories',
  '/admin/runs': '/admin/system/runs',
  '/admin/settings': '/admin/system/about',
  '/admin/settings/security': '/admin/system/security',
  '/admin/settings/notifications': '/admin/system/notifications',
}

function withSearch(pathname: string, params: URLSearchParams): string {
  const search = params.toString()
  return search ? `${pathname}?${search}` : pathname
}

/**
 * Where an old admin URL lives now, or `null` for a current one. Bookmarks, notification
 * links and emails keep working through the 1.x line: every query parameter (`?open=`,
 * `?run=`, filters) survives, except the ones that became part of the path.
 */
export function legacyAdminTarget(path: string, search: string): string | null {
  // `/admin/members/` is the same bookmark as `/admin/members`.
  const pathname = path.length > 1 ? path.replace(/\/+$/, '') : path
  const params = new URLSearchParams(search)
  const member = /^\/admin\/members\/([^/]+)$/.exec(pathname)
  if (member) return withSearch(`/admin/people/members/${member[1]}`, params)
  if (pathname === '/admin/requests') {
    if (params.get('view') !== 'acquisitions') return null
    params.delete('view')
    return withSearch('/admin/downloads', params)
  }
  if (pathname === '/admin/categories') {
    const category = params.get('category')
    params.delete('category')
    const base = MOVED[pathname] as string
    return withSearch(category ? `${base}/${encodeURIComponent(category)}` : base, params)
  }
  const moved = MOVED[pathname]
  return moved ? withSearch(moved, params) : null
}
