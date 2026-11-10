import { useQuery } from '@tanstack/react-query'

import { api } from '@/api/client'
import { queryClient } from '@/api/queryClient'
import type { components } from '@/api/schema'

/**
 * The signed-in account, from `GET /api/v1/auth/me` (cookie session, ADR 0005).
 * `pending` is true until the first answer arrives; guards render nothing then.
 */
export type Role = 'admin' | 'member' | 'none'

export interface Session {
  role: Role
  /** Display name of the signed-in account; `null` when signed out. */
  displayName: string | null
  pending: boolean
  /** The full account (TOTP state, linked member, member view); `null` when signed out. */
  me: Me | null
}

export type Me = components['schemas']['MeResponse']

export const ME_KEY = ['auth', 'me'] as const

async function fetchMe(): Promise<Me | null> {
  const { data, response } = await api.GET('/api/v1/auth/me')
  if (response.status === 401) return null
  if (!data) throw new Error(`auth/me failed: ${response.status}`)
  return data
}

export function useSession(): Session {
  const me = useQuery({ queryKey: ME_KEY, queryFn: fetchMe, staleTime: 60_000, retry: false })
  if (me.isPending) return { role: 'none', displayName: null, pending: true, me: null }
  const account = me.data ?? null
  return {
    role: account?.kind ?? 'none',
    displayName: account?.displayName ?? null,
    pending: false,
    me: account,
  }
}

/** Machine-readable error code from the API, translated by the UI (ADR 0007). */
export type AuthErrorCode = string

export class AuthError extends Error {
  readonly code: AuthErrorCode
  constructor(code: AuthErrorCode) {
    super(code)
    this.code = code
  }
}

function errorCode(error: unknown): AuthErrorCode {
  if (error && typeof error === 'object' && 'code' in error && typeof error.code === 'string') {
    return error.code
  }
  return 'unavailable'
}

/** Outcome of the password step: signed in, or a TOTP code is needed. */
export type SignInResult = { kind: 'signedIn' } | { kind: 'totp'; pendingToken: string }

/**
 * Password step. Admins with TOTP get a pending token for {@link completeTotpSignIn}
 * (no session yet); everyone else is signed in and the guards move to their home.
 */
export async function signIn(username: string, password: string): Promise<SignInResult> {
  const { data, error } = await api.POST('/api/v1/auth/login', { body: { username, password } })
  if (!data) throw new AuthError(errorCode(error))
  if ('pendingToken' in data) return { kind: 'totp', pendingToken: data.pendingToken }
  // Nothing a previous account left in this tab survives a new sign-in.
  queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== ME_KEY[0] })
  queryClient.setQueryData(ME_KEY, data)
  return { kind: 'signedIn' }
}

/** Second step: TOTP code or recovery code for a pending login. */
export async function completeTotpSignIn(pendingToken: string, code: string): Promise<void> {
  const { data, error } = await api.POST('/api/v1/auth/login/totp', {
    body: { pendingToken, code },
  })
  if (!data) throw new AuthError(errorCode(error))
  queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== ME_KEY[0] })
  queryClient.setQueryData(ME_KEY, data)
}

/**
 * The session switched identity (member view, back to admin): forget
 * everything cached for the previous one and use the new account.
 */
export function switchedTo(me: Me): void {
  queryClient.clear()
  queryClient.setQueryData(ME_KEY, me)
}

/** Create the first admin with the setup token printed in the server log; signs in. */
export async function setUpAdmin(input: {
  token: string
  username: string
  password: string
}): Promise<void> {
  const { data, error } = await api.POST('/api/v1/setup/admin', { body: input })
  if (!data) throw new AuthError(errorCode(error))
  queryClient.setQueryData(ME_KEY, data)
  queryClient.setQueryData(SETUP_KEY, false)
}

export const SETUP_KEY = ['setup', 'state'] as const

async function fetchNeedsSetup(): Promise<boolean> {
  const { data, response } = await api.GET('/api/v1/setup/state')
  if (!data) throw new Error(`setup/state failed: ${response.status}`)
  return data.needsSetup
}

/**
 * Whether no admin exists yet (first run), so visitors belong on the setup
 * page. `undefined` until the first answer arrives.
 */
export function useNeedsSetup(): boolean | undefined {
  return useQuery({ queryKey: SETUP_KEY, queryFn: fetchNeedsSetup, staleTime: Infinity }).data
}

/** Forget everything cached for the previous account (lesson 5). */
export function signedOut(): void {
  queryClient.clear()
  queryClient.setQueryData(ME_KEY, null)
}

/** End the session and forget everything cached for this account. */
export async function signOut(): Promise<void> {
  try {
    await api.POST('/api/v1/auth/logout')
  } finally {
    signedOut()
  }
}

/** Query key prefix of a set-password link's check (the set-password page). */
export const PASSWORD_TOKEN_KEY = 'password-token'

/**
 * A token page (invitation, set password) logged someone in: drop whatever a
 * previous account left in the cache, but keep the invitation preview or link
 * check the page is still rendering from (the page drops it when it closes).
 */
export function signedInAs(me: Me): void {
  queryClient.removeQueries({
    predicate: (query) =>
      query.queryKey[0] !== 'invitation' && query.queryKey[0] !== PASSWORD_TOKEN_KEY,
  })
  queryClient.setQueryData(ME_KEY, me)
}

/** Where an admin must go first while TOTP enrollment is required. */
export const SECURITY_PATH = '/admin/system/security'

export function homePath(role: Role): string {
  if (role === 'admin') return '/admin/inbox'
  if (role === 'member') return '/'
  return '/login'
}
