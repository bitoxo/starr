import { screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { ok, renderWithProviders } from '@/test/render'

import SettingsPage from './SettingsPage'

const api = vi.hoisted(() => ({ GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

describe('About (slice 10)', () => {
  it('lists the data sources with their licences', async () => {
    api.GET.mockResolvedValue(ok({ version: '0.9.0', database: 'ok' }))
    renderWithProviders(<SettingsPage />, { path: '/admin/system/about' })
    expect(await screen.findByText('0.9.0')).toBeTruthy()
    expect(screen.getByText(/not endorsed, certified, or otherwise approved by TMDB/)).toBeTruthy()
    expect(screen.getByText(/provided by JustWatch/)).toBeTruthy()
    const links = screen.getAllByRole('link').map((a) => [a.textContent, a.getAttribute('href')])
    expect(links).toContainEqual(['CC0 1.0', 'https://creativecommons.org/publicdomain/zero/1.0/'])
    expect(links).toContainEqual([
      'CC BY-NC-SA 3.0',
      'https://creativecommons.org/licenses/by-nc-sa/3.0/',
    ])
  })
})
