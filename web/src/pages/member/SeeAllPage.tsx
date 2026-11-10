import { ChevronLeftIcon, ListFilterIcon } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useParams, useSearchParams } from 'react-router'
import { z } from 'zod'

import {
  type DiscoverCategory,
  type ListSort,
  type PageList,
  uniqueCards,
  useDiscoverCategories,
  useDiscoverList,
} from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { EmptyState } from '@/components/EmptyState'
import { ErrorNotice } from '@/components/members'
import { Poster } from '@/components/Poster'
import { ProviderLogo, ProviderSwitcher } from '@/components/ProviderSwitcher'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { Button } from '@/components/ui/button'
import { isBrowseCategory } from '@/components/shell/nav'
import { defaultSort, gridFilters, isSeeAllList, listParams } from '@/lib/categoryPage'
import { formatUpcomingDay } from '@/lib/format'
import { useRovingFocus } from '@/lib/roving'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { useUrlState } from '@/lib/urlState'
import { useLoadMore } from '@/lib/useLoadMore'
import { cn } from '@/lib/utils'
import { NotFound } from '@/pages/ErrorPage'

const SORTS: ListSort[] = ['popularity', 'rating', 'release_date']
/** The URL parameters of the filters (F6). */
const FILTER_KEYS = ['provider', 'genre', 'sort', 'hideOwned']

/**
 * `/discover/:category/:list` (spec 0008 Q6 B): every "See all" of a category as a filterable
 * grid. Provider, genre, sort and "Hide what I have" live in the URL (F6, replaced on change);
 * the grid loads more near its end, with a "Load more" button as the fallback.
 */
export default function SeeAllPage() {
  const { category = '', list = '' } = useParams()
  const categories = useDiscoverCategories()
  const errorMessage = useApiErrorMessage()
  const entry = categories.data?.find((c) => c.category === category && c.discoverReady)

  if (categories.isError) return <ErrorNotice>{errorMessage(categories.error)}</ErrorNotice>
  if (categories.isPending) return <GridSkeleton square={false} />
  if (!isBrowseCategory(category) || !entry || !isSeeAllList(entry, list)) return <NotFound />
  return <SeeAllGrid key={`${category}-${list}`} category={entry} list={list} />
}

function GridSkeleton({ square }: { square: boolean }) {
  const { t } = useTranslation('common')
  return (
    <div aria-busy>
      <p role="status" className="sr-only">
        {t('status.loading')}
      </p>
      <ul aria-hidden className={GRID}>
        {Array.from({ length: 12 }, (_, i) => (
          <li key={i}>
            <Skeleton
              className={cn('w-full rounded-card', square ? 'aspect-square' : 'aspect-2/3')}
            />
          </li>
        ))}
      </ul>
    </div>
  )
}

const GRID =
  'grid grid-cols-[repeat(auto-fill,minmax(8.5rem,1fr))] gap-x-4 gap-y-6 sm:grid-cols-[repeat(auto-fill,minmax(10rem,1fr))]'

function SeeAllGrid({ category, list }: { category: DiscoverCategory; list: PageList }) {
  const { t } = useTranslation(['member', 'common'])
  const { open } = useTitleSheet()
  const [, setParams] = useSearchParams()
  const filters = gridFilters(category, list)
  const providerIds = category.providers.map((p) => p.id)
  const genreIds = category.genres.map((g) => g.id)
  // "On <provider>" and a genre always have one; elsewhere "all" is the default.
  const [provider, setProvider] = useUrlState<number>(
    'provider',
    z.coerce
      .number()
      .refine((id) => providerIds.includes(id) || (id === 0 && list !== 'on_provider')),
    list === 'on_provider' ? (providerIds[0] ?? 0) : 0,
  )
  const [genre, setGenre] = useUrlState<string>(
    'genre',
    z.string().refine((g) => genreIds.includes(g)),
    list === 'genre' ? (genreIds[0] ?? '') : '',
  )
  const [sort, setSort] = useUrlState<ListSort>('sort', z.enum(SORTS), defaultSort(list))
  const [hideOwned, setHideOwned] = useUrlState<boolean>(
    'hideOwned',
    z.literal('true').transform(() => true),
    false,
  )
  const chosen = {
    provider: filters.provider ? provider : 0,
    genre: filters.genre ? genre : '',
    sort: filters.sort ? sort : defaultSort(list),
    hideOwned,
  }
  const query = useDiscoverList(category.category, list, listParams(list, chosen))
  const pages = query.data?.pages ?? []
  const cards = uniqueCards(pages)
  const roving = useRovingFocus<HTMLButtonElement>(cards.length, { grid: true })
  const square = category.kind === 'album'
  const name = t(`common:categories.${category.category}`)
  const providerName = category.providers.find((p) => p.id === chosen.provider)?.name
  const genreName = category.genres.find((g) => g.id === chosen.genre)?.name
  const title =
    list === 'on_provider'
      ? t('category.onProvider', { provider: providerName ?? '' })
      : list === 'genre'
        ? (genreName ?? '')
        : list === 'on_the_air'
          ? t('category.onTheAir')
          : t(`discover.lists.${list}`)
  const filtered =
    (list !== 'on_provider' && chosen.provider > 0) ||
    (list !== 'genre' && chosen.genre !== '') ||
    chosen.sort !== defaultSort(list) ||
    hideOwned

  const sentinel = useLoadMore<HTMLDivElement>({
    enabled: query.hasNextPage && !query.isFetchingNextPage && !query.isFetchNextPageError,
    onLoad: () => void query.fetchNextPage(),
    loaded: pages.length,
  })
  // "Load more" by hand: focus goes to the first new title, never to <body> when the button
  // goes away with the last page (F3).
  const focusFrom = useRef<number | null>(null)
  const loadMore = useRef<HTMLButtonElement>(null)
  const retryPage = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    const from = focusFrom.current
    if (from === null || query.isFetchingNextPage) return
    if (query.isFetchNextPageError) {
      // The page failed and "Load more" gave way to the error: its retry takes focus.
      focusFrom.current = null
      retryPage.current?.focus()
    } else if (cards.length > from) {
      focusFrom.current = null
      roving.focus(from)
    } else if (!query.hasNextPage) {
      // The last page held nothing new ("Hide what I have"): the last title instead.
      focusFrom.current = null
      roving.focus(cards.length - 1)
    }
  }, [
    cards.length,
    query.isFetchingNextPage,
    query.isFetchNextPageError,
    query.hasNextPage,
    roving,
  ])

  // "Clear filters" goes away with the filters: focus stays in the toolbar (F3).
  const hideButton = useRef<HTMLButtonElement>(null)
  const clear = () => {
    hideButton.current?.focus()
    setParams(
      (current) => {
        const next = new URLSearchParams(current)
        for (const key of FILTER_KEYS)
          if (key !== 'provider' || list !== 'on_provider') next.delete(key)
        // A genre grid keeps its genre; the default one stays out of the URL.
        if (list === 'genre' && chosen.genre !== genreIds[0]) next.set('genre', chosen.genre)
        return next
      },
      { replace: true },
    )
  }

  return (
    <div className="flex flex-col gap-6">
      <header className="flex flex-col gap-2">
        <Link
          to={`/discover/${category.category}`}
          aria-label={t('category.grid.backLabel', { category: name })}
          className="-ml-1 inline-flex h-8 items-center gap-1 self-start rounded-full pr-2 pl-1 text-sm text-text-2 hover:text-text max-lg:h-11"
        >
          <ChevronLeftIcon aria-hidden className="size-4" />
          {t('category.grid.back', { category: name })}
        </Link>
        <h1 className="type-display flex items-center gap-3 text-3xl font-extrabold text-balance text-text sm:text-4xl">
          {list === 'on_provider' && (
            <ProviderLogo
              logo={category.providers.find((p) => p.id === chosen.provider)?.logo}
              name={providerName ?? ''}
              className="size-9 rounded-card"
            />
          )}
          {title}
        </h1>
      </header>

      <div
        role="group"
        aria-label={t('category.grid.filters')}
        className="flex flex-col gap-3 border-b border-line pb-4"
      >
        {filters.provider && (
          <ProviderSwitcher
            label={t('category.providers')}
            chosen={String(chosen.provider)}
            onChoose={(id) => setProvider(Number(id))}
            options={[
              ...(list === 'on_provider'
                ? []
                : [{ id: '0', name: t('category.grid.allProviders') }]),
              ...category.providers.map((p) => ({
                id: String(p.id),
                name: p.name,
                image: <ProviderLogo logo={p.logo} name={p.name} />,
              })),
            ]}
          />
        )}
        {filters.genre && (
          <ProviderSwitcher
            label={t('category.genre')}
            chosen={chosen.genre === '' ? '-' : chosen.genre}
            onChoose={(id) => setGenre(id === '-' ? '' : id)}
            options={[
              ...(list === 'genre' ? [] : [{ id: '-', name: t('category.grid.allGenres') }]),
              ...category.genres.map((g) => ({ id: g.id, name: g.name })),
            ]}
          />
        )}
        <div className="flex flex-wrap items-center gap-3">
          {filters.sort && (
            <label className="flex items-center gap-2 text-sm text-text-2">
              {t('category.grid.sort')}
              <select
                value={chosen.sort}
                onChange={(e) => setSort(e.target.value as ListSort)}
                className="h-9 cursor-pointer rounded-full border border-line bg-night-1 px-3 text-sm text-text max-lg:h-11"
              >
                {SORTS.map((s) => (
                  <option key={s} value={s}>
                    {t(`category.grid.sortBy.${s}`)}
                  </option>
                ))}
              </select>
            </label>
          )}
          <button
            ref={hideButton}
            type="button"
            aria-pressed={hideOwned}
            onClick={() => setHideOwned(!hideOwned)}
            className={cn(
              'inline-flex h-9 cursor-pointer items-center gap-2 rounded-full px-3.5 text-sm font-medium ring-1 transition-colors duration-(--dur-1) ring-inset max-lg:h-11',
              hideOwned
                ? 'bg-night-2 text-text ring-text-3'
                : 'text-text-2 ring-line hover:text-text',
            )}
          >
            <span
              aria-hidden
              className={cn('size-1.5 rounded-full', hideOwned ? 'bg-text' : 'bg-line')}
            />
            {t('category.grid.hideOwned')}
          </button>
          {filtered && (
            <Button variant="ghost" size="sm" className="max-lg:h-11" onClick={clear}>
              {t('category.grid.clear')}
            </Button>
          )}
        </div>
      </div>

      <p aria-live="polite" className="type-data text-xs text-text-3">
        {query.isSuccess && t('category.grid.shown', { count: cards.length })}
      </p>

      {query.isPending && <GridSkeleton square={square} />}
      {query.isError && !query.data && (
        <QueryError message={t('discover.listError')} onRetry={() => void query.refetch()} />
      )}
      {query.isSuccess && cards.length === 0 && !query.hasNextPage && (
        <EmptyState
          icon={ListFilterIcon}
          title={t('category.grid.emptyTitle')}
          description={filtered ? t('category.grid.emptyFiltered') : t('discover.listEmpty')}
          action={
            filtered && (
              <Button size="sm" onClick={clear}>
                {t('category.grid.clear')}
              </Button>
            )
          }
        />
      )}
      {cards.length > 0 && (
        <ul
          id="see-all-grid"
          aria-label={t('category.lane', { category: name, lane: title })}
          aria-busy={query.isFetching}
          onKeyDown={roving.onKeyDown}
          className={GRID}
        >
          {cards.map((card, i) => (
            <li key={`${card.kind}-${card.id}`}>
              <Poster
                card={card}
                onOpen={open}
                roving={roving.itemProps(i)}
                caption
                detail={
                  list === 'upcoming' && card.releaseDate
                    ? formatUpcomingDay(card.releaseDate)
                    : undefined
                }
                sizes="(min-width: 640px) 176px, 136px"
              />
            </li>
          ))}
        </ul>
      )}
      <div ref={sentinel} aria-hidden />
      {query.isFetchingNextPage && <GridSkeleton square={square} />}
      {query.isFetchNextPageError && (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-3 rounded-card border border-dashed border-line px-5 py-6"
        >
          <p className="text-base text-text-2">{t('category.grid.pageError')}</p>
          <Button
            ref={retryPage}
            size="sm"
            onClick={() => {
              // On success the button goes away: focus to the first new title then.
              focusFrom.current = cards.length
              void query.fetchNextPage()
            }}
          >
            {t('common:actions.retry')}
          </Button>
        </div>
      )}
      {query.hasNextPage && !query.isFetchNextPageError && (
        <Button
          ref={loadMore}
          variant="secondary"
          className="self-center max-lg:h-11"
          aria-disabled={query.isFetchingNextPage || undefined}
          onClick={() => {
            if (query.isFetchingNextPage) return
            focusFrom.current = cards.length
            void query.fetchNextPage()
          }}
        >
          {t('category.grid.loadMore')}
        </Button>
      )}
    </div>
  )
}
