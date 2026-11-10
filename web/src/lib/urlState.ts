import { useSearchParams } from 'react-router'
import { z } from 'zod'

/*
 * URL state (spec 0008 F6): whatever a user expects after reload, Back or a shared link lives in
 * the query string. One parameter per value, validated with zod; a missing or invalid value
 * falls back silently to the default, and the default is not written to the URL. Filter changes
 * replace the history entry, opening a page or detail pushes one. Private terms belong in URLs
 * under `/private` only (spec 0007).
 */

export function useUrlState<T extends string | number | boolean>(
  key: string,
  schema: z.ZodType<T>,
  fallback: T,
): [T, (next: T, options?: { push?: boolean }) => void] {
  const [params, setParams] = useSearchParams()
  const raw = params.get(key)
  const parsed = raw === null ? null : schema.safeParse(raw)
  const value = parsed?.success ? parsed.data : fallback

  const set = (next: T, { push = false }: { push?: boolean } = {}) =>
    setParams(
      (current) => {
        const updated = new URLSearchParams(current)
        if (next === fallback) updated.delete(key)
        else updated.set(key, String(next))
        return updated
      },
      { replace: !push },
    )

  return [value, set]
}

/** A table sort as one URL parameter: `name` ascending, `-name` descending. */
export type UrlSort = { column: string; direction: 'asc' | 'desc' } | null

const SORT = z.string().regex(/^-?[a-zA-Z]+$/)

export function parseSort(raw: string): UrlSort {
  if (!raw) return null
  return raw.startsWith('-')
    ? { column: raw.slice(1), direction: 'desc' }
    : { column: raw, direction: 'asc' }
}

export function formatSort(sort: UrlSort): string {
  if (!sort) return ''
  return sort.direction === 'desc' ? `-${sort.column}` : sort.column
}

/** The sort of an admin table in `?sort=` (F6); a `DataTable` takes both values as they are. */
export function useUrlSort(fallback: UrlSort = null): [UrlSort, (next: UrlSort) => void] {
  const [raw, setRaw] = useUrlState('sort', SORT, formatSort(fallback))
  return [parseSort(raw), (next) => setRaw(formatSort(next))]
}
