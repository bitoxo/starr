import type { SeasonDetails, TitleRef } from '@/api/discover'

/** Why a season can't be requested: it is there, or I asked for it already. */
export type SeasonBlock = 'available' | 'mine'

export function seasonBlock(season: SeasonDetails): SeasonBlock | null {
  if (season.status === 'available') return 'available'
  if (season.requestedByMe) return 'mine'
  return null
}

/** Regular seasons (no specials) that can still be requested. */
export function selectableSeasons(seasons: SeasonDetails[]): number[] {
  return seasons.filter((s) => !s.special && seasonBlock(s) === null).map((s) => s.number)
}

/** "Select all": all open regular seasons, or none once all of them are chosen. */
export function toggleAll(seasons: SeasonDetails[], selected: Set<number>): Set<number> {
  const all = selectableSeasons(seasons)
  if (all.length > 0 && all.every((n) => selected.has(n))) return new Set()
  return new Set([...selected, ...all])
}

/**
 * `movie-27205`: how the open detail view is kept in the URL (`?title=`). The first `-` ends
 * the kind; the rest is the catalog's opaque id (UUIDs carry dashes of their own).
 */
export function titleRefParam(ref: TitleRef): string {
  return `${ref.kind}-${ref.id}`
}

export function parseTitleRef(raw: string | null): TitleRef | null {
  const match = raw?.match(/^(movie|series|artist|album)-([A-Za-z0-9][A-Za-z0-9._:-]{0,63})$/)
  if (!match) return null
  return { kind: match[1] as TitleRef['kind'], id: match[2] }
}
