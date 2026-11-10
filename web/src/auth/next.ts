import { homePath, type Role } from '@/auth/session'
import { inboxLink } from '@/lib/attention'

/*
 * Where sign-in leads (spec 0008 AUTH-02, F6): a signed-out visitor goes to `/login?next=<where
 * they were>` and comes back there after signing in. `next` is untrusted input (anyone can send
 * a login link), so it is only ever followed as a relative path inside this app: no other
 * origin, no `//` or `\` tricks, no sign-in or token page, and only into the account's own area.
 * The private area (spec 0007) never shows up in a URL outside `/private`: any private path
 * becomes plain `/private`, which opens only after unlocking, as always.
 */

const MAX_LENGTH = 2048
/** Stand-in origin to resolve `next` against; anything resolving elsewhere is refused. */
const BASE = 'https://starr.invalid'
const PRIVATE = '/private'
const AUTH_PAGES = ['/login', '/setup', '/forgot-password', '/invite', '/set-password']

/** `path` is `prefix` itself or below it (`/private`, `/private/…`, not `/privateer`). */
function within(path: string, prefix: string): boolean {
  return path === prefix || path.startsWith(`${prefix}/`)
}

/**
 * The path as the router matches it: percent-decoded and lower case (React Router matches
 * case-insensitively), or `null` when it cannot be decoded.
 */
function matchable(pathname: string): string | null {
  try {
    return decodeURIComponent(pathname).toLowerCase()
  } catch {
    return null
  }
}

/** Control characters (C0 and DEL), which browsers strip or reinterpret when resolving. */
function hasControl(value: string): boolean {
  for (let i = 0; i < value.length; i += 1) {
    const code = value.charCodeAt(i)
    if (code < 0x20 || code === 0x7f) return true
  }
  return false
}

/**
 * Fail-closed: a path that cannot be decoded counts as private, because React Router then
 * matches the raw path, and `/private/x%E0?q=…` would still open the private route.
 */
function isPrivate(pathname: string): boolean {
  const path = matchable(pathname)
  return path === null || within(path, PRIVATE) || within(pathname.toLowerCase(), PRIVATE)
}

/** `raw` as an in-app path for `role` to land on after signing in, or `null` to go home. */
export function safeNext(raw: string | null, role: Exclude<Role, 'none'>): string | null {
  if (!raw || raw.length > MAX_LENGTH) return null
  // Only a plain absolute path: no scheme, no `//host`, no backslash, no control characters or
  // whitespace browsers would strip before resolving.
  if (!raw.startsWith('/') || raw.includes('\\') || /\s/.test(raw) || hasControl(raw)) return null
  let url: URL
  try {
    url = new URL(raw, BASE)
  } catch {
    return null
  }
  if (url.origin !== BASE) return null
  const path = matchable(url.pathname)
  // Dot segments and encodings can still produce `//host` or `\` once resolved.
  if (path === null || path.startsWith('//') || path.includes('\\')) return null
  if (AUTH_PAGES.some((page) => within(path, page))) return null
  if (within(path, '/admin') !== (role === 'admin')) return null
  if (within(path, PRIVATE)) return PRIVATE
  return `${url.pathname}${url.search}`
}

/** The login page for a signed-out visitor at `location`, remembering where they were. */
export function loginPath(location: { pathname: string; search: string }): string {
  if (location.pathname === '/' && !location.search) return '/login'
  const next = isPrivate(location.pathname) ? PRIVATE : `${location.pathname}${location.search}`
  return `/login?${new URLSearchParams({ next }).toString()}`
}

/**
 * Where a guest page sends an account that just signed in (or arrived signed in): after
 * first-run setup the Inbox with the setup checklist open, else a valid `next`, else home.
 */
export function afterSignIn(role: Exclude<Role, 'none'>, pathname: string, search: string) {
  if (pathname.toLowerCase() === '/setup' && role === 'admin') return inboxLink('setup')
  return safeNext(new URLSearchParams(search).get('next'), role) ?? homePath(role)
}
