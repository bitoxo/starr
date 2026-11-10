import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { HeldAcquisitions, Service, ServiceType } from '@/api/services'
import { fail, ok, renderWithProviders } from '@/test/render'

import ServicesPage from './ServicesPage'

const api = vi.hoisted(() => ({ GET: vi.fn(), PUT: vi.fn(), POST: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))
const toast = vi.hoisted(() => Object.assign(vi.fn(), { error: vi.fn() }))
vi.mock('sonner', () => ({ toast }))

const SONARR: Service = {
  id: 2,
  adapterType: 'sonarr',
  name: 'Sonarr',
  baseUrl: 'http://sonarr:8989',
  publicUrl: null,
  mode: 'live',
  detectedVersion: '4.0.20',
  capabilities: ['requests'],
  lastCheckAt: '2026-11-02T10:00:00Z',
  lastError: null,
  writesPausedAt: '2026-11-02T09:00:00Z',
  writesPausedBy: 'root',
  createdAt: '2026-11-01T10:00:00Z',
  updatedAt: '2026-11-02T09:00:00Z',
}

const TYPES = [
  { typeId: 'sonarr', displayName: 'Sonarr', capabilities: ['requests'] },
] as unknown as ServiceType[]

function serve(held: HeldAcquisitions) {
  api.GET.mockImplementation((path: string) => {
    if (path === '/api/v1/services') return Promise.resolve(ok([SONARR]))
    if (path === '/api/v1/service-types') return Promise.resolve(ok(TYPES))
    if (path === '/api/v1/services/{id}/held') return Promise.resolve(ok(held))
    return Promise.resolve(ok([]))
  })
}

const resumeSwitch = () => screen.findByRole('switch', { name: 'Writes to Sonarr' })

describe('Services: resuming writes', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.PUT.mockReset()
    toast.error.mockReset()
    api.PUT.mockResolvedValue(ok({ ...SONARR, writesPausedAt: null, writesPausedBy: null }))
  })

  it('resumes at once when nothing is held', async () => {
    serve({ total: 0, preview: [], stopTotal: 0, stops: [] })
    renderWithProviders(<ServicesPage />)
    fireEvent.click(await resumeSwitch())
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/services/{id}/writes-paused', {
        params: { path: { id: 2 } },
        body: { paused: false },
      }),
    )
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('names what goes out and resumes only after confirming', async () => {
    serve({
      total: 7,
      preview: [
        { title: 'Severance', year: 2022, season: 3, redacted: false },
        { title: 'Dark', year: 2017, season: 1, redacted: false },
        { title: 'Andor', year: 2022, season: 2, redacted: false },
        { title: 'Shogun', year: 2024, season: 1, redacted: false },
        { title: 'Slow Horses', year: 2022, season: 5, redacted: false },
      ],
      stopTotal: 0,
      stops: [],
    })
    renderWithProviders(<ServicesPage />)
    fireEvent.click(await resumeSwitch())
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByText('7 approved titles will go to Sonarr now:')).toBeTruthy()
    expect(within(dialog).getByText('Severance S03')).toBeTruthy()
    expect(within(dialog).getByText('Slow Horses S05')).toBeTruthy()
    expect(within(dialog).getByText('+ 2 more')).toBeTruthy()
    expect(
      within(dialog)
        .getAllByRole('button')
        .map((b) => b.textContent),
    ).toEqual(expect.arrayContaining(['Cancel', 'Resume and send']))
    expect(api.PUT).not.toHaveBeenCalled()

    fireEvent.click(within(dialog).getByRole('button', { name: 'Resume and send' }))
    await waitFor(() =>
      expect(api.PUT).toHaveBeenCalledWith('/api/v1/services/{id}/writes-paused', {
        params: { path: { id: 2 } },
        body: { paused: false },
      }),
    )
  })

  it('lists held stops as "stop monitoring" lines', async () => {
    serve({
      total: 0,
      preview: [],
      stopTotal: 1,
      stops: [{ title: 'Dark', year: 2017, season: 2, redacted: false }],
    })
    renderWithProviders(<ServicesPage />)
    fireEvent.click(await resumeSwitch())
    const dialog = await screen.findByRole('dialog')
    expect(
      within(dialog).getByText('Starr only turns off monitoring it switched on in Sonarr.'),
    ).toBeTruthy()
    expect(within(dialog).getByText('Stop monitoring')).toBeTruthy()
    expect(within(dialog).getByText('Dark S02')).toBeTruthy()
    expect(api.PUT).not.toHaveBeenCalled()
  })

  it('cancelling keeps the writes paused', async () => {
    serve({
      total: 1,
      preview: [{ title: 'The Matrix', year: 1999, season: null, redacted: false }],
      stopTotal: 0,
      stops: [],
    })
    renderWithProviders(<ServicesPage />)
    fireEvent.click(await resumeSwitch())
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByText('1 approved title will go to Sonarr now:')).toBeTruthy()
    expect(within(dialog).getByText('The Matrix')).toBeTruthy()
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(api.PUT).not.toHaveBeenCalled()
  })

  it('stays paused and says so when the held titles cannot be read', async () => {
    serve({ total: 0, preview: [], stopTotal: 0, stops: [] })
    const served = api.GET.getMockImplementation() as (path: string) => Promise<unknown>
    api.GET.mockImplementation((path: string) =>
      path === '/api/v1/services/{id}/held' ? Promise.resolve(fail('internal', 500)) : served(path),
    )
    renderWithProviders(<ServicesPage />)
    const toggle = await resumeSwitch()
    fireEvent.click(toggle)
    await waitFor(() => expect(toast.error).toHaveBeenCalled())
    expect(api.PUT).not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog')).toBeNull()
    expect(toggle.getAttribute('aria-checked')).toBe('false')
  })
})
