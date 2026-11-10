import { describe, expect, it } from 'vitest'

import { isElevated } from './elevation'

describe('isElevated (ADM-34)', () => {
  const now = Date.parse('2026-11-07T12:00:00Z')

  it('needs a time in the future, beyond the margin', () => {
    expect(isElevated(null, 0, now)).toBe(false)
    expect(isElevated('2026-11-07T11:59:00Z', 0, now)).toBe(false)
    expect(isElevated('2026-11-07T12:04:00Z', 60_000, now)).toBe(true)
    // Thirty seconds left: too little to pick an account in the dialog.
    expect(isElevated('2026-11-07T12:00:30Z', 60_000, now)).toBe(false)
  })
})
