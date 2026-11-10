import { CompassIcon, SearchIcon, SearchXIcon } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { z } from 'zod'

import { type TitleCard, uniqueCards, useDiscoverCategories, useSearch } from '@/api/discover'
import { Button } from '@/components/ui/button'
import { useApiErrorMessage } from '@/api/errors'
import { PageHeader } from '@/components/AppShell'
import { CategoryDot } from '@/components/CategoryDot'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { EmptyState } from '@/components/EmptyState'
import { ErrorNotice } from '@/components/members'
import { Poster } from '@/components/Poster'
import { type BrowseCategory, isBrowseCategory } from '@/components/shell/nav'
import { searchGroups } from '@/lib/music'
import { useUrlState } from '@/lib/urlState'
import { useLoadMore } from '@/lib/useLoadMore'
import { cn } from '@/lib/utils'

const GRID =
  'grid grid-cols-2 gap-x-4 gap-y-7 min-[480px]:grid-cols-3 md:grid-cols-4 xl:grid-cols-5'

type Filter = BrowseCategory | 'all'
const FILTER = z.enum(['all', 'movies', 'series', 'music'])

function inCategory(card: TitleCard, filter: Filter): boolean {
  switch (filter) {
    case 'all':
      return true
    case 'movies':
      return card.kind === 'movie'
    case 'series':
      return card.kind === 'series'
    case 'music':
      return card.kind === 'artist' || card.kind === 'album'
  }
}

/** The results' shape while the first page loads: a grid of cover placeholders (MEM-13). */
function ResultsSkeleton() {
  const { t } = useTranslation('common')
  return (
    <div aria-busy>
      <p role="status" className="sr-only">
        {t('status.loading')}
      </p>
      <ul aria-hidden className={GRID}>
        {Array.from({ length: 10 }, (_, i) => (
          <li key={i} className="flex flex-col gap-2">
            <div className="aspect-[2/3] rounded-card bg-night-1 motion-safe:animate-pulse" />
            <div className="h-4 w-3/4 rounded-control bg-night-1 motion-safe:animate-pulse" />
          </li>
        ))}
      </ul>
    </div>
  )
}

/**
 * `/search?q=&category=` (spec 0008 IA): results as a grid with captions and a category marker
 * per card; a category filter in the URL (F6) for the categories the member may browse; empty
 * states that lead somewhere (MEM-17). Search covers no private titles (spec 0007).
 */
export default function SearchPage() {
  const { t } = useTranslation(['member', 'common'])
  const [params] = useSearchParams()
  const query = (params.get('q') ?? '').trim()
  const [chosen, setFilter] = useUrlState<Filter>('category', FILTER, 'all')
  const search = useSearch(query)
  const errorMessage = useApiErrorMessage()
  const { open } = useTitleSheet()
  const browsable = (useDiscoverCategories().data ?? [])
    .filter((c) => c.discoverReady)
    .map((c) => c.category as string)
    .filter(isBrowseCategory)
  // A filter for a category the member cannot browse shows everything.
  const filter: Filter = chosen !== 'all' && browsable.includes(chosen) ? chosen : 'all'
  const pages = search.data?.pages ?? []
  const all = uniqueCards(pages)
  const cards = all.filter((card) => inCategory(card, filter))
  const groups = searchGroups(cards)
  // Headings only once music is in the results.
  const headed = groups.some(([g]) => g !== 'video')
  // After a failed page, wait for "Try again" instead of asking in a loop.
  // A filter that matches nothing loaded yet does not page through the whole search by
  // itself: the empty state offers "Load more results" instead.
  const sentinel = useLoadMore<HTMLDivElement>({
    enabled:
      search.hasNextPage &&
      !search.isFetchingNextPage &&
      !search.isFetchNextPageError &&
      (filter === 'all' || cards.length > 0),
    onLoad: () => void search.fetchNextPage(),
    loaded: pages.length,
  })
  const categoryName = (c: BrowseCategory) => t(`common:categories.${c}`)
  // "Show all results" goes away with the empty state: focus moves to the "All" filter.
  const allFilter = useRef<HTMLButtonElement>(null)
  const focusAll = useRef(false)
  useEffect(() => {
    if (!focusAll.current) return
    focusAll.current = false
    allFilter.current?.focus()
  })

  return (
    <>
      <PageHeader
        variant="member"
        title={query ? t('search.resultsFor', { query }) : t('search.title')}
      />
      {query && browsable.length > 1 && all.length > 0 && (
        <div
          role="group"
          aria-label={t('search.filterLabel')}
          className="-mx-1 -mt-4 mb-8 flex gap-1 overflow-x-auto px-1 py-1 [scrollbar-width:none]"
        >
          {(['all', ...browsable] as Filter[]).map((f) => (
            <button
              key={f}
              ref={f === 'all' ? allFilter : undefined}
              type="button"
              aria-pressed={f === filter}
              onClick={() => setFilter(f)}
              className={cn(
                'flex h-8 shrink-0 cursor-pointer items-center gap-2 rounded-full px-3.5 text-sm font-medium whitespace-nowrap text-text-2 transition-colors duration-(--dur-1) hover:text-text max-lg:h-11 max-lg:px-4',
                f === filter
                  ? 'bg-night-2 text-text ring-1 ring-line ring-inset'
                  : 'hover:bg-night-1',
              )}
            >
              {f !== 'all' && <CategoryDot category={f} className="size-1.5" />}
              {f === 'all' ? t('search.filterAll') : categoryName(f)}
            </button>
          ))}
        </div>
      )}
      {!query && (
        <EmptyState
          icon={SearchIcon}
          title={t('search.noQueryTitle')}
          description={t('search.noQueryDescription')}
        >
          {browsable.length > 0 && (
            <nav aria-label={t('search.noQueryBrowse')} className="flex flex-col gap-2">
              <p className="text-sm text-text-3">{t('search.noQueryBrowse')}</p>
              <ul className="flex flex-wrap gap-2">
                {browsable.map((c) => (
                  <li key={c}>
                    <Button asChild variant="secondary" size="sm" className="max-lg:h-11">
                      <Link to={`/discover/${c}`}>
                        <CategoryDot category={c} className="size-1.5" />
                        {categoryName(c)}
                      </Link>
                    </Button>
                  </li>
                ))}
              </ul>
            </nav>
          )}
        </EmptyState>
      )}
      {search.isError && !search.data && (
        <ErrorNotice>
          {t('search.error')} {errorMessage(search.error)}
        </ErrorNotice>
      )}
      {query && search.isPending && <ResultsSkeleton />}
      {search.isSuccess && all.length === 0 && (
        <EmptyState
          icon={SearchXIcon}
          title={t('search.noResultsTitle')}
          description={t('search.noResultsDescription', { query })}
          action={
            <Button asChild variant="secondary" className="max-lg:h-11">
              <Link to="/">
                <CompassIcon aria-hidden />
                {t('search.backToDiscover')}
              </Link>
            </Button>
          }
        />
      )}
      {search.isSuccess &&
        all.length > 0 &&
        cards.length === 0 &&
        filter !== 'all' &&
        !search.isFetchingNextPage && (
          <EmptyState
            icon={SearchXIcon}
            title={t(`search.filteredEmptyTitle_${filter}`, { query })}
            description={
              search.hasNextPage
                ? t('search.filteredEmptyMore')
                : t('search.filteredEmptyDescription')
            }
            action={
              <div className="flex flex-wrap gap-2">
                {search.hasNextPage && (
                  <Button
                    variant="secondary"
                    className="max-lg:h-11"
                    onClick={() => void search.fetchNextPage()}
                  >
                    {t('search.loadMore')}
                  </Button>
                )}
                <Button
                  variant="secondary"
                  className="max-lg:h-11"
                  onClick={() => {
                    setFilter('all')
                    focusAll.current = true
                  }}
                >
                  {t('search.showAll')}
                </Button>
              </div>
            }
          />
        )}
      <div className="flex flex-col gap-12" aria-busy={search.isFetching}>
        {groups.map(([group, items]) => (
          <section
            key={group}
            aria-labelledby={headed ? `results-${group}` : undefined}
            className="flex flex-col gap-4"
          >
            {headed && (
              <h2 id={`results-${group}`} className="type-display text-xl font-bold text-text">
                {t(`search.groups.${group}`)}
              </h2>
            )}
            <ul className={GRID}>
              {items.map((card) => (
                <li key={`${card.kind}-${card.id}`}>
                  <Poster
                    card={card}
                    onOpen={open}
                    sizes="(min-width: 1280px) 200px, (min-width: 768px) 25vw, 45vw"
                  />
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
      <div ref={sentinel} aria-hidden className="h-px" />
      {search.isFetchingNextPage && (
        <p className="mt-6 text-text-3">{t('common:status.loading')}</p>
      )}
      {search.isFetchNextPageError && (
        <div className="mt-6 flex flex-wrap items-center gap-3">
          <p className="text-base text-text-2">{t('search.error')}</p>
          <Button size="sm" onClick={() => void search.fetchNextPage()}>
            {t('common:actions.retry')}
          </Button>
        </div>
      )}
    </>
  )
}
