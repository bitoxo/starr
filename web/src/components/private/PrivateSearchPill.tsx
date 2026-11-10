import { SearchIcon, XIcon } from 'lucide-react'
import { useEffect, useRef, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useSearchParams } from 'react-router'

import { searchPath } from '@/lib/privateNav'

/** Wait this long after the last keystroke before searching (spec 0008 PRIV-12). */
export const PRIVATE_DEBOUNCE_MS = 250

/**
 * The header search inside the unlocked private area (spec 0008 IA, PRIV-12): the same pill as
 * the global search, but it searches scenes, performers and studios, live after 250 ms, and
 * shows its results on `/private/search?q=`. A new text drops the studio or performer filter.
 * Rendered only on `/private…` while unlocked; the text lives only in this component and in
 * URLs under `/private` (F6), so leaving the area forgets it.
 */
export function PrivateSearchPill() {
  const { t } = useTranslation('member')
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const [params] = useSearchParams()
  const onResults = pathname === '/private/search'
  const urlQuery = onResults ? (params.get('q') ?? '') : ''
  const [value, setValue] = useState(urlQuery)
  const [synced, setSynced] = useState(urlQuery)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  const input = useRef<HTMLInputElement>(null)

  // Follow the URL (Back/Forward), keeping what is typed when it means the same search.
  if (synced !== urlQuery) {
    setSynced(urlQuery)
    if (value.trim() !== urlQuery.trim()) setValue(urlQuery)
  }
  useEffect(() => () => clearTimeout(timer.current), [pathname])

  const go = (text: string) => {
    clearTimeout(timer.current)
    const q = text.trim()
    if (onResults && q === urlQuery.trim()) return
    // Typing on the results page replaces its entry; arriving there pushes one.
    void navigate(searchPath(q), { replace: onResults })
  }

  const onChange = (text: string) => {
    setValue(text)
    clearTimeout(timer.current)
    if (!text.trim() && !onResults) return
    timer.current = setTimeout(() => go(text), PRIVATE_DEBOUNCE_MS)
  }

  const onSubmit = (event: FormEvent) => {
    event.preventDefault()
    go(value)
  }

  return (
    <form role="search" onSubmit={onSubmit} className="relative w-full max-w-xl">
      <SearchIcon
        aria-hidden
        className="pointer-events-none absolute top-1/2 left-4 size-4 -translate-y-1/2 text-text-3"
        strokeWidth={1.75}
      />
      <input
        ref={input}
        type="search"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        aria-label={t('private.searchLabel')}
        placeholder={t('private.searchPlaceholder')}
        maxLength={200}
        enterKeyHint="search"
        autoComplete="off"
        className="h-11 w-full rounded-full border border-line bg-night-1 pr-11 pl-11 text-base text-text placeholder:text-text-3 hover:border-text-3 focus-visible:border-transparent [&::-webkit-search-cancel-button]:hidden"
      />
      {value && (
        <button
          type="button"
          onClick={() => {
            setValue('')
            if (onResults) go('')
            input.current?.focus()
          }}
          className="absolute top-1/2 right-0 grid size-11 -translate-y-1/2 cursor-pointer place-items-center rounded-full text-text-3 hover:bg-night-2 hover:text-text"
        >
          <XIcon aria-hidden className="size-4" />
          <span className="sr-only">{t('search.clear')}</span>
        </button>
      )}
    </form>
  )
}
