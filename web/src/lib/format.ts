import i18n from 'i18next'

/*
 * Every date, time, number and duration the UI shows goes through here (spec 0008 F7, ADR 0007
 * as amended): `Intl` in the **active i18n language**, so an English UI never shows German dates
 * or "3,5 TB" because of the browser. Only the hour cycle (24 h or 12 h) follows the user's own
 * convention, read from the browser locale. A test keeps `toLocaleString()` and own `Intl`
 * formatters out of every other file.
 */

/** The language the UI is shown in. */
function language(): string {
  return i18n.resolvedLanguage ?? i18n.language ?? 'en'
}

const formatters = new Map<string, unknown>()

/** One cached formatter per language and options. */
function cached<T>(kind: string, options: object, make: (lang: string) => T): T {
  const lang = language()
  const key = `${kind}|${lang}|${JSON.stringify(options)}`
  let formatter = formatters.get(key) as T | undefined
  if (!formatter) {
    formatter = make(lang)
    formatters.set(key, formatter)
  }
  return formatter
}

function dateFormat(options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  return cached('date', options, (lang) => new Intl.DateTimeFormat(lang, options))
}

function numberFormat(options: Intl.NumberFormatOptions = {}): Intl.NumberFormat {
  return cached('number', options, (lang) => new Intl.NumberFormat(lang, options))
}

/** The user's hour cycle ("21:14" or "9:14 PM"), from the browser's own locale. */
function hourCycle(): Intl.DateTimeFormatOptions['hourCycle'] {
  const browser = typeof navigator === 'undefined' ? undefined : navigator.language
  try {
    return new Intl.DateTimeFormat(browser, { hour: 'numeric' }).resolvedOptions().hourCycle
  } catch {
    return undefined
  }
}

/** A calendar day (`2026-11-03`) as a local date, without time zone shifts; null if invalid. */
function localDay(day: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})/.exec(day)
  if (!match) return null
  const date = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]))
  return Number.isNaN(date.getTime()) ? null : date
}

/** "Nov 6, 2026". */
export function formatDate(iso: string): string {
  return dateFormat({ dateStyle: 'medium' }).format(new Date(iso))
}

/** A calendar day (`2026-11-03`) as "Nov 3, 2026"; an unparsable value stays as it came. */
export function formatDay(day: string): string {
  const date = localDay(day)
  return date ? dateFormat({ dateStyle: 'medium' }).format(date) : day
}

/** An upcoming calendar day as an exact weekday: "Fri, Nov 13". */
export function formatUpcomingDay(day: string): string {
  const date = localDay(day)
  return date ? dateFormat({ weekday: 'short', month: 'short', day: 'numeric' }).format(date) : day
}

/** "Nov 6": a day of the current stretch (journey steps). */
export function formatDayMonth(iso: string): string {
  return dateFormat({ month: 'short', day: 'numeric' }).format(new Date(iso))
}

/** "21:14" or "9:14 PM", in the user's hour cycle. */
export function formatTime(iso: string): string {
  return dateFormat({ hour: 'numeric', minute: '2-digit', hourCycle: hourCycle() }).format(
    new Date(iso),
  )
}

/** "Nov 6, 2026, 21:14". */
export function formatDateTime(iso: string): string {
  return dateFormat({ dateStyle: 'medium', timeStyle: 'short', hourCycle: hourCycle() }).format(
    new Date(iso),
  )
}

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['day', 86_400],
  ['hour', 3_600],
  ['minute', 60],
]

/** "5 minutes ago", "in 3 days", "now". */
export function formatRelative(iso: string, now: Date = new Date()): string {
  const relative = cached(
    'relative',
    { numeric: 'auto' },
    (lang) => new Intl.RelativeTimeFormat(lang, { numeric: 'auto' }),
  )
  const seconds = Math.round((new Date(iso).getTime() - now.getTime()) / 1000)
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit)
  }
  return relative.format(0, 'second')
}

/** Compares two texts as the UI language sorts them (numbers inside by value). */
export function compareText(a: string, b: string): number {
  return cached(
    'collator',
    { numeric: true },
    (lang) => new Intl.Collator(lang, { numeric: true, sensitivity: 'base' }),
  ).compare(a, b)
}

/** A count or decimal: "12,345.5". */
export function formatNumber(value: number, options?: Intl.NumberFormatOptions): string {
  return numberFormat(options).format(value)
}

/** A fraction 0–1 as a whole percentage: "35%". */
export function formatPercent(fraction: number): string {
  return numberFormat({ style: 'percent', maximumFractionDigits: 0 }).format(
    Math.min(1, Math.max(0, fraction)),
  )
}

/** A byte count in the largest fitting binary unit: "3.5 TB", "512 MB". */
export function formatBytes(bytes: number): string {
  const units = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte'] as const
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return numberFormat({
    style: 'unit',
    unit: units[unit],
    unitDisplay: 'short',
    maximumFractionDigits: value < 10 ? 1 : 0,
  }).format(value)
}

/** A running time in whole minutes: "38 min", "1 h 12 min", "2 h" (never "1:12"). */
export function formatMinutes(total: number): string {
  const minutes = Math.max(0, Math.round(total))
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  const n = (value: number) => formatNumber(value)
  if (hours === 0) return i18n.t('format.minutes', { minutes: n(rest) })
  if (rest === 0) return i18n.t('format.hours', { hours: n(hours) })
  return i18n.t('format.hoursMinutes', { hours: n(hours), minutes: n(rest) })
}

/** A running time given in seconds (scenes, tracks): minutes, or "45 s" below one. */
export function formatSeconds(total: number): string {
  if (total < 60) {
    return i18n.t('format.seconds', {
      seconds: formatNumber(Math.max(0, Math.round(total))),
    })
  }
  return formatMinutes(total / 60)
}

/** A track's length: "4 min 23 s", "3 min", "42 s" (words, never "4:23"; F7). */
export function formatTrackLength(total: number): string {
  const seconds = Math.max(0, Math.round(total))
  if (seconds < 60) return i18n.t('format.seconds', { seconds: formatNumber(seconds) })
  const minutes = Math.floor(seconds / 60)
  const rest = seconds % 60
  if (rest === 0) return i18n.t('format.minutes', { minutes: formatNumber(minutes) })
  return i18n.t('format.minutesSeconds', {
    minutes: formatNumber(minutes),
    seconds: formatNumber(rest),
  })
}

/** A language code (ISO 639-1) by name in the UI language: `ja` → "Japanese". */
export function formatLanguage(code: string): string {
  if (!code) return code
  try {
    return (
      cached('language', {}, (lang) => new Intl.DisplayNames(lang, { type: 'language' })).of(
        code,
      ) ?? code
    )
  } catch {
    // Not a valid code: show it as the catalog gave it.
    return code
  }
}

/** Compact time between two instants (job runs): "850 ms", "12.3 s", "4 min 2 s". */
export function formatElapsed(fromIso: string, toIso: string): string {
  const ms = Math.max(0, new Date(toIso).getTime() - new Date(fromIso).getTime())
  if (ms < 1000) return i18n.t('format.milliseconds', { ms: formatNumber(ms) })
  if (ms < 60_000) {
    return i18n.t('format.seconds', {
      seconds: formatNumber(ms / 1000, { maximumFractionDigits: 1 }),
    })
  }
  return i18n.t('format.minutesSeconds', {
    minutes: formatNumber(Math.floor(ms / 60_000)),
    seconds: formatNumber(Math.round((ms % 60_000) / 1000)),
  })
}

const BROWSERS: [string, string][] = [
  ['Edg/', 'Edge'],
  ['Firefox/', 'Firefox'],
  ['Chrome/', 'Chrome'],
  ['Safari/', 'Safari'],
]
const SYSTEMS: [RegExp, string][] = [
  [/iPhone|iPad/, 'iOS'],
  [/Android/, 'Android'],
  [/Mac OS X/, 'macOS'],
  [/Windows/, 'Windows'],
  [/Linux/, 'Linux'],
]

/** A short, readable device label from a User-Agent string, or null. */
export function deviceLabel(userAgent: string | null | undefined): string | null {
  if (!userAgent) return null
  const browser = BROWSERS.find(([marker]) => userAgent.includes(marker))?.[1]
  const os = SYSTEMS.find(([pattern]) => pattern.test(userAgent))?.[1]
  if (browser && os) return `${browser} · ${os}`
  return browser ?? os ?? userAgent.slice(0, 40)
}
