import i18n from 'i18next'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  deviceLabel,
  formatBytes,
  formatDate,
  formatDateTime,
  formatDay,
  formatDayMonth,
  formatElapsed,
  formatLanguage,
  formatMinutes,
  formatNumber,
  formatPercent,
  formatRelative,
  formatSeconds,
  formatTime,
  formatTrackLength,
  formatUpcomingDay,
} from '@/lib/format'

// Local wall-clock instants, so the tests hold in every time zone.
const at = (y: number, mo: number, d: number, h = 0, mi = 0) =>
  new Date(y, mo - 1, d, h, mi).toISOString()

function browserLocale(locale: string) {
  vi.spyOn(navigator, 'language', 'get').mockReturnValue(locale)
}

afterEach(async () => {
  vi.restoreAllMocks()
  await i18n.changeLanguage('en')
})

describe('format.ts follows the active i18n language, not the browser (MEM-11)', () => {
  it('writes English dates in an English UI on a German browser', () => {
    browserLocale('de-DE')
    expect(formatDate(at(2026, 11, 6, 21, 14))).toBe('Nov 6, 2026')
    expect(formatDay('2026-11-03')).toBe('Nov 3, 2026')
    expect(formatDayMonth(at(2026, 11, 6, 8, 2))).toBe('Nov 6')
  })

  it('writes upcoming days as exact weekdays', () => {
    expect(formatUpcomingDay('2026-11-13')).toBe('Fri, Nov 13')
  })

  it('keeps an unparsable calendar day as it came', () => {
    expect(formatDay('soon')).toBe('soon')
  })

  it('formats numbers, percentages and sizes in the UI language', () => {
    browserLocale('de-DE')
    expect(formatNumber(12345.5)).toBe('12,345.5')
    expect(formatPercent(0.35)).toBe('35%')
    expect(formatPercent(1.4)).toBe('100%')
    expect(formatBytes(3.5 * 1024 ** 4)).toBe('3.5 TB')
    expect(formatBytes(512 * 1024 ** 2)).toBe('512 MB')
  })

  it('formats relative times', () => {
    const now = new Date(2026, 10, 6, 12, 0)
    expect(formatRelative(at(2026, 11, 6, 11, 55), now)).toBe('5 minutes ago')
    expect(formatRelative(at(2026, 11, 9, 12, 0), now)).toBe('in 3 days')
    expect(formatRelative(now.toISOString(), now)).toBe('now')
  })

  it('switches with the i18n language', async () => {
    // A second UI language, as one would ship later (ADR 0007).
    i18n.addResourceBundle('de', 'common', { format: { minutes: '{{minutes}} Min.' } })
    await i18n.changeLanguage('de')
    expect(formatDate(at(2026, 11, 6))).toBe('06.11.2026')
    expect(formatNumber(1234.5)).toBe('1.234,5')
  })
})

describe('times use the user’s hour cycle', () => {
  it('24 h on a browser that writes 24 h', () => {
    browserLocale('de-DE')
    expect(formatTime(at(2026, 11, 6, 21, 14))).toBe('21:14')
    expect(formatDateTime(at(2026, 11, 6, 21, 14))).toBe('Nov 6, 2026, 21:14')
  })

  it('12 h on a browser that writes 12 h', () => {
    browserLocale('en-US')
    expect(formatTime(at(2026, 11, 6, 21, 14))).toMatch(/^9:14\sPM$/)
  })
})

describe('durations are words, never a clock (F7)', () => {
  it('minutes', () => {
    expect(formatMinutes(38)).toBe('38 min')
    expect(formatMinutes(72)).toBe('1 h 12 min')
    expect(formatMinutes(120)).toBe('2 h')
    expect(formatMinutes(0)).toBe('0 min')
  })

  it('seconds round to minutes, short ones stay seconds', () => {
    expect(formatSeconds(22 * 60 + 2)).toBe('22 min')
    expect(formatSeconds(3900)).toBe('1 h 5 min')
    expect(formatSeconds(45)).toBe('45 s')
  })

  it('track lengths keep their seconds, still in words', () => {
    expect(formatTrackLength(4 * 60 + 23)).toBe('4 min 23 s')
    expect(formatTrackLength(3 * 60)).toBe('3 min')
    expect(formatTrackLength(42)).toBe('42 s')
  })

  it('elapsed time between two instants', () => {
    const from = '2026-11-06T10:00:00.000Z'
    const plus = (ms: number) => new Date(Date.parse(from) + ms).toISOString()
    expect(formatElapsed(from, plus(850))).toBe('850 ms')
    expect(formatElapsed(from, plus(12_340))).toBe('12.3 s')
    expect(formatElapsed(from, plus(242_000))).toBe('4 min 2 s')
    expect(formatElapsed(from, plus(-5))).toBe('0 ms')
  })
})

describe('formatLanguage', () => {
  it('names a language in the UI language, keeping unknown codes', () => {
    expect(formatLanguage('ja')).toBe('Japanese')
    expect(formatLanguage('en')).toBe('English')
    expect(formatLanguage('')).toBe('')
  })
})

describe('deviceLabel', () => {
  it('names browser and system', () => {
    expect(
      deviceLabel(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15',
      ),
    ).toBe('Safari · macOS')
    expect(deviceLabel(null)).toBeNull()
  })
})

describe('no locale formatting outside format.ts', () => {
  // Every date, number and duration goes through format.ts, so nothing can pick the
  // browser locale by accident (F7: "no toLocaleString() without the language").
  const sources = import.meta.glob<string>(['/src/**/*.{ts,tsx}', '!/src/**/*.test.{ts,tsx}'], {
    query: '?raw',
    import: 'default',
    eager: true,
  })

  it('finds the sources', () => {
    expect(Object.keys(sources).length).toBeGreaterThan(50)
  })

  it('has no toLocaleString() or own Intl formatter anywhere else', () => {
    const offenders = Object.entries(sources)
      .filter(([path]) => !path.endsWith('/lib/format.ts'))
      .filter(([, source]) => /\.toLocale(Date|Time)?String\(|new Intl\./.test(source))
      .map(([path]) => path)
    expect(offenders).toEqual([])
  })
})
