import { SearchIcon, XIcon } from 'lucide-react'
import { useEffect, useRef, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useSearchParams } from 'react-router'

/** Wait this long after the last keystroke before searching. */
const DEBOUNCE_MS = 300

/**
 * Global search for members: a pill that searches as you type and shows results on `/search`.
 * Search ignores the relevance (D3) but, like Discover, covers only categories that are
 * set up for the member.
 */
export function SearchPill() {
  const { t } = useTranslation('member')
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const [params] = useSearchParams()
  const onSearchPage = pathname === '/search'
  const urlQuery = onSearchPage ? (params.get('q') ?? '') : ''
  const [value, setValue] = useState(urlQuery)
  const [synced, setSynced] = useState(urlQuery)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  const input = useRef<HTMLInputElement>(null)

  // Follow the URL (Back/Forward, leaving the results page), but keep what is being typed
  // (e.g. a trailing space) when it means the same search.
  if (synced !== urlQuery) {
    setSynced(urlQuery)
    if (value.trim() !== urlQuery.trim()) setValue(urlQuery)
  }
  // Navigating elsewhere (Back, a nav link) drops a search still waiting to fire.
  useEffect(() => () => clearTimeout(timer.current), [pathname])

  const go = (text: string) => {
    clearTimeout(timer.current)
    const q = text.trim()
    if (q === urlQuery.trim() && onSearchPage) return
    // The category filter of the results page stays while the text changes (spec 0008 F6).
    const category = onSearchPage ? params.get('category') : null
    const query = [
      q && `q=${encodeURIComponent(q)}`,
      category && `category=${encodeURIComponent(category)}`,
    ]
      .filter(Boolean)
      .join('&')
    navigate(query ? `/search?${query}` : '/search', { replace: onSearchPage })
  }

  const onChange = (text: string) => {
    setValue(text)
    clearTimeout(timer.current)
    if (!text.trim() && !onSearchPage) return
    timer.current = setTimeout(() => go(text), DEBOUNCE_MS)
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
        aria-label={t('search.label')}
        placeholder={t('search.placeholder')}
        maxLength={200}
        enterKeyHint="search"
        className="h-11 w-full rounded-full border border-line bg-night-1 pr-11 pl-11 text-base text-text placeholder:text-text-3 hover:border-text-3 focus-visible:border-transparent [&::-webkit-search-cancel-button]:hidden"
      />
      {value && (
        <button
          type="button"
          onClick={() => {
            setValue('')
            if (onSearchPage) go('')
            // The clear button goes away: focus stays in the search (MEM-03).
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
