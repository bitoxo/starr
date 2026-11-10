import { createContext, useCallback, useContext, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

/*
 * What a view inside the detail overlay (spec 0008 A7) may set on the stage around it: the
 * ambient layer behind the dialog, state that outlives one view while the overlay is open, and
 * the overlay's own close.
 */

export type DetailStage = {
  /** The ambient layer behind the dialog (movie and series backdrops, A1); null clears it. */
  setAmbient: (src: string | null) => void
  /** State that outlives the view while the overlay stays open (artist ↔ album, MEM-29). */
  memory: Map<string, unknown>
  /** Closes the overlay ("not found" has its own button). */
  close: () => void
}

export const DetailStageContext = createContext<DetailStage | null>(null)

/** The overlay's close, for buttons inside a view. */
export function useDetailClose(): () => void {
  return useContext(DetailStageContext)?.close ?? (() => {})
}

/** Sets the ambient layer behind the overlay while the calling view shows. */
export function useDetailAmbient(src: string | null) {
  const stage = useContext(DetailStageContext)
  useEffect(() => {
    stage?.setAmbient(src)
    return () => stage?.setAmbient(null)
  }, [stage, src])
}

/**
 * Like `useState`, but kept while the overlay is open: going from an artist to an album and
 * back finds the ticks and "Show all" as they were (MEM-29). Cleared when the overlay closes.
 */
export function useDetailMemory<T>(key: string, initial: T): [T, (next: T) => void] {
  const stage = useContext(DetailStageContext)
  const [value, setValue] = useState<T>(() =>
    stage?.memory.has(key) ? (stage.memory.get(key) as T) : initial,
  )
  const set = useCallback(
    (next: T) => {
      stage?.memory.set(key, next)
      setValue(next)
    },
    [stage, key],
  )
  return [value, set]
}

/** Keeps `document.title` on the open title ("Inception · Starr"), restoring it on close. */
export function useDetailDocumentTitle(title: string) {
  const { t } = useTranslation()
  const full = t('app.pageTitle', { page: title, app: t('app.name') })
  useEffect(() => {
    const before = document.title
    document.title = full
    return () => {
      document.title = before
    }
  }, [full])
}
