import { fireEvent, screen, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ok, renderWithProviders } from '@/test/render'

import MembersPage from './MembersPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

function member(id: number, username: string, extra: object = {}) {
  return {
    id,
    username,
    displayName: username[0].toUpperCase() + username.slice(1),
    email: `${username}@example.org`,
    status: 'active',
    passwordSet: true,
    createdAt: `2026-11-0${id}T08:00:00Z`,
    entitlements: [
      { category: 'movies', enabled: true, requestMode: 'approval' },
      { category: 'series', enabled: true, requestMode: 'auto' },
    ],
    ...extra,
  }
}

const MEMBERS = [member(1, 'ben', { status: 'suspended' }), member(2, 'anna'), member(3, 'cara')]

function names() {
  return screen
    .getAllByRole('link')
    .map((a) => a.textContent ?? '')
    .map((text) => text.split(/[a-z]+@/)[0])
}

describe('Members (ADM-25)', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    api.GET.mockResolvedValue(ok(MEMBERS))
  })

  it('reads search, status and sort from the URL and clears them in one go', async () => {
    renderWithProviders(<MembersPage />, {
      path: '/admin/people/members?status=active&sort=-member',
    })
    await screen.findByText('Cara')
    expect(names().map((n) => n.slice(0, 4))).toEqual(['Cara', 'Anna'])
    expect(screen.getByRole('radio', { name: 'Active' }).getAttribute('aria-checked')).toBe('true')
    fireEvent.click(screen.getByRole('button', { name: 'Clear filters' }))
    expect(await screen.findAllByRole('link')).toHaveLength(3)
  })

  it('labels both request modes', async () => {
    renderWithProviders(<MembersPage />, { path: '/admin/people/members' })
    const row = (await screen.findByText('Anna')).closest('tr') as HTMLElement
    expect(within(row).getByText('Approval')).toBeTruthy()
    expect(within(row).getByText('Auto')).toBeTruthy()
  })

  it('warns when a new member gets no category (ADM-14)', async () => {
    renderWithProviders(<MembersPage />, { path: '/admin/people/members' })
    fireEvent.click(await screen.findByRole('button', { name: 'Add member' }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByRole('note').textContent).toMatch(/cannot request anything/)
    fireEvent.click(within(dialog).getByRole('switch', { name: 'Movies enabled' }))
    expect(within(dialog).queryByRole('note')).toBeNull()
  })

  it('asks before giving a new member NSFW (ADM-12)', async () => {
    renderWithProviders(<MembersPage />, { path: '/admin/people/members' })
    fireEvent.click(await screen.findByRole('button', { name: 'Add member' }))
    const dialog = await screen.findByRole('dialog')
    fireEvent.click(within(dialog).getByRole('switch', { name: 'NSFW enabled' }))
    const confirm = await screen.findByRole('alertdialog', {
      name: 'Switch NSFW on for this member?',
    })
    expect(confirm.textContent).toMatch(/only for adults who want it/)
  })
})
