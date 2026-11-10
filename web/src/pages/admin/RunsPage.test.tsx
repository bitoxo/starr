import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ok, renderWithProviders } from '@/test/render'

import RunsPage from './RunsPage'

const api = vi.hoisted(() => ({ GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

function run(id: number, kind: string, extra: object = {}) {
  return {
    id,
    jobId: id,
    kind,
    trigger: 'schedule',
    attempt: 1,
    state: 'succeeded',
    expectation: 'met',
    startedAt: '2026-11-07T08:00:00Z',
    finishedAt: '2026-11-07T08:00:02Z',
    ...extra,
  }
}

const DETAIL = {
  ...run(7, 'sources.lists', { state: 'failed', expectation: null, error: 'HTTP 500: boom' }),
  report: null,
  transitions: [],
}

describe('Runs (ADM-29)', () => {
  beforeEach(() => api.GET.mockReset())

  it('names every job kind readably and reads the filters from the URL', async () => {
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(
        path === '/api/v1/runs'
          ? ok({ runs: [run(9, 'catalog.refresh-lists'), run(8, 'accounts.password')] })
          : ok(DETAIL),
      ),
    )
    renderWithProviders(<RunsPage />, {
      path: '/admin/system/runs?job=accounts.password&outcome=failed',
    })
    expect(await screen.findByRole('button', { name: /Music genre lanes/ })).toBeTruthy()
    expect(screen.getByRole('button', { name: /Account passwords/ })).toBeTruthy()
    expect(api.GET).toHaveBeenCalledWith('/api/v1/runs', {
      params: { query: { kind: 'accounts.password', outcome: 'failed', limit: 50 } },
    })
    // No raw kind name in the job filter.
    const select = screen.getByRole('combobox', { name: 'Job' })
    const labels = within(select)
      .getAllByRole('option')
      .map((o) => o.textContent ?? '')
    // All jobs plus the 13 kinds of `JobKind::ALL`.
    expect(labels).toHaveLength(14)
    expect(labels.some((l) => l.includes('.'))).toBe(false)
    fireEvent.click(screen.getByRole('button', { name: 'Clear filters' }))
    await waitFor(() =>
      expect(api.GET).toHaveBeenLastCalledWith('/api/v1/runs', {
        params: { query: { limit: 50 } },
      }),
    )
  })

  it('offers more runs page by page', async () => {
    api.GET.mockResolvedValueOnce(ok({ runs: [run(9, 'system.noop')], nextBefore: 9 }))
    api.GET.mockResolvedValueOnce(ok({ runs: [run(3, 'system.noop')] }))
    renderWithProviders(<RunsPage />, { path: '/admin/system/runs' })
    fireEvent.click(await screen.findByRole('button', { name: 'Load more' }))
    expect(await screen.findByRole('button', { name: /#3/ })).toBeTruthy()
    expect(api.GET).toHaveBeenLastCalledWith('/api/v1/runs', {
      params: { query: { limit: 50, before: 9 } },
    })
  })

  it('explains a failed run in words and keeps the raw text collapsed', async () => {
    api.GET.mockImplementation((path: string) =>
      Promise.resolve(path === '/api/v1/runs' ? ok({ runs: [] }) : ok(DETAIL)),
    )
    renderWithProviders(<RunsPage />, { path: '/admin/system/runs?run=7' })
    expect(await screen.findByText(/The run failed/)).toBeTruthy()
    const raw = screen.getByText('HTTP 500: boom')
    expect(raw.closest('details')?.open).toBe(false)
  })
})
