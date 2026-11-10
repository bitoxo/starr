import { describe, expect, it } from 'vitest'

import { afterSignIn, loginPath, safeNext } from './next'

describe('safeNext (spec 0008 AUTH-02)', () => {
  it('keeps relative in-app paths with their query', () => {
    expect(safeNext('/requests?state=open', 'member')).toBe('/requests?state=open')
    expect(safeNext('/discover/movies?title=movie-27205', 'member')).toBe(
      '/discover/movies?title=movie-27205',
    )
    expect(safeNext('/admin/inbox?open=approval%3A12', 'admin')).toBe(
      '/admin/inbox?open=approval%3A12',
    )
  })

  it.each([
    ['protocol-relative', '//evil.example/x'],
    ['backslash host', '/\\evil.example'],
    ['backslash anywhere', '/requests\\..\\x'],
    ['absolute https', 'https://evil.example/'],
    ['absolute same-looking', 'http://localhost:7979/requests'],
    ['javascript scheme', 'javascript:alert(1)'],
    ['data scheme', 'data:text/html,hi'],
    ['no leading slash', 'requests'],
    ['tab trick', '/\t/evil.example'],
    ['newline trick', '/\n/evil.example'],
    ['space before', ' //evil.example'],
    ['dot segment to //', '/.//evil.example'],
    ['traversal to //', '/a/..//evil.example'],
    ['encoded slashes', '/%2F%2Fevil.example'],
    ['encoded backslash', '/%5Cevil.example'],
    ['undecodable', '/private/x%E0%A4?q=term'],
    ['empty', ''],
    ['too long', `/${'a'.repeat(2100)}`],
  ])('refuses %s', (_, raw) => {
    expect(safeNext(raw, 'member')).toBeNull()
    expect(safeNext(raw, 'admin')).toBeNull()
  })

  it('refuses a missing value', () => {
    expect(safeNext(null, 'member')).toBeNull()
  })

  it("only leads into the account's own area", () => {
    expect(safeNext('/admin/people/members', 'member')).toBeNull()
    expect(safeNext('/ADMIN/people', 'member')).toBeNull()
    expect(safeNext('/requests', 'admin')).toBeNull()
    expect(safeNext('/admin', 'admin')).toBe('/admin')
    expect(safeNext('/administrator', 'admin')).toBeNull()
  })

  it('never leads back to a sign-in or token page', () => {
    for (const path of [
      '/login',
      '/login?next=/requests',
      '/setup',
      '/forgot-password',
      '/invite/abc',
      '/set-password/abc',
    ]) {
      expect(safeNext(path, 'member')).toBeNull()
    }
  })

  it('reduces any private path to the closed area, never a private term', () => {
    for (const path of [
      '/private',
      '/private/search?q=term',
      '/private/performers/42',
      '/PRIVATE/search?q=term',
      '/%70rivate/search?q=term',
      '/requests/../private/search?q=term',
    ]) {
      expect(safeNext(path, 'member')).toBe('/private')
    }
    expect(safeNext('/privateer', 'member')).toBe('/privateer')
  })
})

describe('loginPath', () => {
  const at = (pathname: string, search = '') => ({ pathname, search })

  it('carries where the visitor was as ?next=', () => {
    expect(loginPath(at('/requests', '?state=open'))).toBe('/login?next=%2Frequests%3Fstate%3Dopen')
    expect(loginPath(at('/admin/inbox'))).toBe('/login?next=%2Fadmin%2Finbox')
  })

  it('leaves the home page out', () => {
    expect(loginPath(at('/'))).toBe('/login')
  })

  it('never puts a private path or term into the URL', () => {
    expect(loginPath(at('/private/search', '?q=term'))).toBe('/login?next=%2Fprivate')
    expect(loginPath(at('/Private/performers/42'))).toBe('/login?next=%2Fprivate')
    // A path that cannot be decoded is still matched raw by the router: fail closed.
    expect(loginPath(at('/private/search%E0%A4', '?q=term'))).toBe('/login?next=%2Fprivate')
    expect(loginPath(at('/requests%E0%A4', '?q=x'))).toBe('/login?next=%2Fprivate')
  })
})

describe('afterSignIn', () => {
  it('follows a valid next, else goes home', () => {
    expect(afterSignIn('member', '/login', '?next=%2Frequests')).toBe('/requests')
    expect(afterSignIn('member', '/login', '?next=%2F%2Fevil.example')).toBe('/')
    expect(afterSignIn('admin', '/login', '?next=%2Frequests')).toBe('/admin/inbox')
    expect(afterSignIn('member', '/login', '')).toBe('/')
  })

  it('opens the setup checklist in the Inbox after first-run setup', () => {
    expect(afterSignIn('admin', '/setup', '')).toBe('/admin/inbox?open=setup')
    expect(afterSignIn('admin', '/setup', '?next=%2Fadmin%2Fpeople')).toBe(
      '/admin/inbox?open=setup',
    )
  })
})
