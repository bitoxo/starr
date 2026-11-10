import { describe, expect, it } from 'vitest'

import { accountChips } from './accounts'

const base: Parameters<typeof accountChips>[0] = {
  goneSince: null,
  missingRuns: 0,
  ignoredAt: null,
  privileged: false,
  lockedByService: false,
  lockedByStarr: false,
  enabled: true,
}

const chips = (overrides: Partial<typeof base> & Record<string, unknown>) =>
  accountChips({ ...base, ...overrides }).map((c) => c.chip)

describe('accountChips', () => {
  it('shows the server flag of a present account', () => {
    expect(chips({})).toEqual(['enabled'])
    expect(chips({ enabled: false })).toEqual(['disabled'])
    expect(chips({ enabled: false, lockedByService: true })).toEqual(['locked'])
  })

  it("shows Starr's lock where the server has no off switch", () => {
    // Navidrome reports the locked account as enabled.
    expect(chips({ lockedByStarr: true })).toEqual(['lockedByStarr'])
    expect(chips({ privileged: true })).toEqual(['admin', 'enabled'])
  })

  it('puts presence and decisions before the flag', () => {
    expect(chips({ privileged: true })).toEqual(['admin', 'enabled'])
    expect(chips({ missingRuns: 1, ignoredAt: '2026-11-02T10:00:00Z' })).toEqual([
      'missing',
      'ignored',
      'enabled',
    ])
  })

  it('shows no server flag for a gone account', () => {
    expect(chips({ goneSince: '2026-11-02T10:00:00Z', missingRuns: 2 })).toEqual(['gone'])
  })
})
