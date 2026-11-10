import { afterEach, describe, expect, it, vi } from 'vitest'

import { plaqueDetail } from '@/lib/plaque'
import type { JourneyUnit } from '@/lib/states'

const unit = (
  steps: JourneyUnit['journey']['steps'],
  library: string | null = null,
): JourneyUnit => ({ journey: { steps, library } })

// Times at 10:00 UTC are the same calendar day from UTC−10 to UTC+13.
describe('plaqueDetail (spec 0008 A4, amendment 2026-11-07)', () => {
  afterEach(() => vi.useRealTimers())

  it('names the server, and since when only from a journey that arrived there', () => {
    vi.useFakeTimers({ now: new Date('2026-11-07T10:00:00Z') })
    const ready = [{ step: 'ready', status: 'done', at: '2026-11-03T10:00:00Z' }] as const
    expect(plaqueDetail({ key: 'available' }, [], 'Jellyfin')).toBe('in Jellyfin')
    expect(plaqueDetail({ key: 'available' }, [unit([...ready], 'Jellyfin')], 'Jellyfin')).toBe(
      'in Jellyfin since Nov 3',
    )
    // Another server's arrival says nothing about this one.
    expect(plaqueDetail({ key: 'available' }, [unit([...ready], 'Emby')], 'Jellyfin')).toBe(
      'in Jellyfin',
    )
    expect(plaqueDetail({ key: 'available' }, [unit([...ready], 'Jellyfin')])).toBeNull()
    expect(plaqueDetail({ key: 'partlyAvailable' }, [], 'Jellyfin')).toBe('in Jellyfin')
  })

  it('gives an old date its year', () => {
    vi.useFakeTimers({ now: new Date('2026-11-07T10:00:00Z') })
    const old = unit([{ step: 'requested', status: 'done', at: '2025-11-02T10:00:00Z' }])
    expect(plaqueDetail({ key: 'awaitingApproval' }, [old])).toBe('requested Nov 2, 2025')
  })

  it('says when a step began, or nothing when it is unknown', () => {
    vi.useFakeTimers({ now: new Date('2026-11-07T10:00:00Z') })
    const searching = unit([
      { step: 'requested', status: 'done', at: '2026-11-01T10:00:00Z' },
      { step: 'searching', status: 'current', at: '2026-11-05T10:00:00Z' },
    ])
    const importing = unit([{ step: 'importing', status: 'current', at: '2026-11-06T10:00:00Z' }])
    expect(plaqueDetail({ key: 'searching' }, [searching])).toBe('since Nov 5')
    expect(plaqueDetail({ key: 'importing' }, [importing])).toBe('since Nov 6')
    expect(plaqueDetail({ key: 'searching' }, [])).toBeNull()
    expect(plaqueDetail({ key: 'requestedByOthers' }, [searching])).toBeNull()
    expect(plaqueDetail({ key: 'available' }, [])).toBeNull()
    expect(plaqueDetail({ key: 'downloading', percent: 0.62 }, [])).toBe('62% downloaded')
    expect(plaqueDetail({ key: 'downloading' }, [])).toBeNull()
  })
})
