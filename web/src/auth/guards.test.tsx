import { render, screen } from '@testing-library/react'
import { createMemoryRouter, RouterProvider } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { GuestOnly, RequireRole } from './guards'
import type { Session } from './session'

const session = vi.hoisted(() => ({
  current: { role: 'none', displayName: null, pending: false, me: null } as Session,
  needsSetup: false as boolean | undefined,
}))

vi.mock('./session', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./session')>()),
  useSession: () => session.current,
  useNeedsSetup: () => session.needsSetup,
}))

function renderAt(path: string) {
  const router = createMemoryRouter(
    [
      {
        element: <GuestOnly />,
        children: [
          { path: '/login', element: <p>login page</p> },
          { path: '/setup', element: <p>setup page</p> },
        ],
      },
      {
        element: <RequireRole role="member" />,
        children: [
          { path: '/', element: <p>member home</p> },
          { path: '/requests', element: <p>member requests</p> },
          { path: '/private/*', element: <p>private area</p> },
        ],
      },
      {
        element: <RequireRole role="admin" />,
        children: [
          { path: '/admin/inbox', element: <p>admin inbox</p> },
          { path: '/admin/members', element: <p>admin members</p> },
          { path: '/admin/system/security', element: <p>security page</p> },
        ],
      },
    ],
    { initialEntries: [path] },
  )
  render(<RouterProvider router={router} />)
  return router
}

const as = (role: Session['role'], pending = false, totpEnrollmentRequired = false) => {
  session.current = {
    role,
    displayName: role === 'none' ? null : 'Mia',
    pending,
    me:
      role === 'none'
        ? null
        : {
            kind: role,
            username: 'mia',
            displayName: 'Mia',
            totpEnabled: false,
            totpEnrollmentRequired,
          },
  }
}

describe('route guards', () => {
  beforeEach(() => {
    as('none')
    session.needsSetup = false
  })

  it('show the shell skeleton while the session is loading (MEM-13)', () => {
    as('none', true)
    const router = renderAt('/admin/members')
    expect(router.state.location.pathname).toBe('/admin/members')
    expect(screen.queryByText('admin members')).toBeNull()
    expect(document.querySelector('[data-shell-skeleton]')).not.toBeNull()
    expect(screen.getByRole('status').textContent).toBe('Loading Starr…')
  })

  it('send signed-out visitors to the login page', async () => {
    const router = renderAt('/admin/members')
    expect(await screen.findByText('login page')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/login')
  })

  it('keep members out of the admin area', async () => {
    as('member')
    const router = renderAt('/admin/members')
    expect(await screen.findByText('member home')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/')
  })

  it('keep admins out of the member area and land them on the Inbox (ADM-22)', async () => {
    as('admin')
    const router = renderAt('/')
    expect(await screen.findByText('admin inbox')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/admin/inbox')
  })

  it('let each role into its own area', async () => {
    as('admin')
    renderAt('/admin/members')
    expect(await screen.findByText('admin members')).toBeTruthy()
  })

  it('send admins who must enroll TOTP to the security page', async () => {
    as('admin', false, true)
    const router = renderAt('/admin/members')
    expect(await screen.findByText('security page')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/admin/system/security')
  })

  it('move signed-in accounts away from the login page', async () => {
    as('member')
    const router = renderAt('/login')
    expect(await screen.findByText('member home')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/')
  })

  it('send visitors to setup while no admin exists', async () => {
    session.needsSetup = true
    const router = renderAt('/admin/members')
    expect(await screen.findByText('setup page')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/setup')
  })

  it('send visitors from setup to login once setup is done', async () => {
    const router = renderAt('/setup')
    expect(await screen.findByText('login page')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/login')
  })

  it('render nothing while the setup state is loading', () => {
    session.needsSetup = undefined
    renderAt('/login')
    expect(screen.queryByText('login page')).toBeNull()
  })

  it('remember where a signed-out visitor was, as ?next= (AUTH-02)', async () => {
    const router = renderAt('/requests?state=open')
    expect(await screen.findByText('login page')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/login')
    expect(new URLSearchParams(router.state.location.search).get('next')).toBe(
      '/requests?state=open',
    )
  })

  it('never carry a private path or term into the login URL', async () => {
    const router = renderAt('/private/search?q=secret-term')
    expect(await screen.findByText('login page')).toBeTruthy()
    expect(router.state.location.search).toBe('?next=%2Fprivate')
    expect(router.state.location.search).not.toContain('secret')
  })

  it('fail closed on a private path that cannot be decoded', async () => {
    const router = renderAt('/private/search%E0%A4?q=secret-term')
    expect(await screen.findByText('login page')).toBeTruthy()
    expect(router.state.location.search).toBe('?next=%2Fprivate')
  })

  it('bring a member back to next after signing in', async () => {
    as('member')
    const router = renderAt('/login?next=%2Frequests%3Fstate%3Dopen')
    expect(await screen.findByText('member requests')).toBeTruthy()
    expect(router.state.location.pathname).toBe('/requests')
    expect(router.state.location.search).toBe('?state=open')
  })

  it.each(['//evil.example/', 'https://evil.example/', '/%2F%2Fevil.example', '/admin/members'])(
    'ignore an unsafe or foreign next (%s) and go home',
    async (next) => {
      as('member')
      const router = renderAt(`/login?next=${encodeURIComponent(next)}`)
      expect(await screen.findByText('member home')).toBeTruthy()
      expect(router.state.location.pathname).toBe('/')
    },
  )

  it('open the setup checklist in the Inbox after first-run setup', async () => {
    as('admin')
    const router = renderAt('/setup')
    expect(await screen.findByText('admin inbox')).toBeTruthy()
    expect(router.state.location.search).toBe('?open=setup')
  })
})
