import {
  ArrowLeftIcon,
  ArrowRightIcon,
  HourglassIcon,
  SearchIcon,
  SearchXIcon,
  XIcon,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import {
  isNotFound,
  MAX_SCENES,
  type PrivateCard,
  type PrivateFollow,
  type PrivateList,
  privateImage,
  type SearchFilter,
  usePrivateArea,
  usePrivateEntity,
  usePrivateFollows,
  usePrivateLane,
  usePrivateScene,
  usePrivateSearch,
} from '@/api/private'
import { Ambient } from '@/components/Ambient'
import { CategoryDot } from '@/components/CategoryDot'
import { EmptyState } from '@/components/EmptyState'
import { PrivateFollowControl } from '@/components/PrivateFollows'
import { EntityShape, EntityTileBody, FollowingChip } from '@/components/private/EntityTile'
import { type CaptionMode, SceneGrid, SelectionBar } from '@/components/private/SceneGrid'
import { SceneSkeleton, SceneView } from '@/components/private/SceneView'
import { useSceneSelection } from '@/lib/sceneSelection'
import { QueryError } from '@/components/QueryError'
import { Button } from '@/components/ui/button'
import { formatNumber } from '@/lib/format'
import { entityPath, searchPath, usePrivateBack } from '@/lib/privateNav'
import { useRovingFocus } from '@/lib/roving'
import { useUrlState } from '@/lib/urlState'
import { cn } from '@/lib/utils'
import { NotFound } from '@/pages/ErrorPage'
import { PrivatePageSkeleton } from '@/pages/member/PrivateLayout'

/*
 * The private category's own area (spec 0007; UI per spec 0008 slice 7, Q7 A with C's filter
 * chip): browse as a YouTube-like scene grid (Trending by default, or New), the area's search
 * (studio lane, performer lane, scene grid with counts and an entity filter chip), and the
 * scene, performer and studio pages, all inside `PrivateArea`. The browser title stays neutral;
 * private terms appear only in URLs under `/private` (F6). While the gate is closed every
 * request answers 404 and the area is the plain "not found".
 */

function RequestsClosed() {
  const { t } = useTranslation('member')
  return <p className="text-sm text-text-3">{t('private.requestsClosed')}</p>
}

/** The private heading row: dot, title, and what follows it (pills, actions). */
function AreaHeading({ children, description }: { children: ReactNode; description?: string }) {
  return (
    <div className="mb-8 flex flex-col gap-1.5">
      <h1 className="type-display flex items-center gap-3 text-3xl font-extrabold text-text sm:text-4xl">
        <CategoryDot category="adult" className="size-2.5" />
        {children}
      </h1>
      {description && <p className="text-base text-text-2">{description}</p>}
    </div>
  )
}

const LIST = z.enum(['trending', 'new'])

/** Pills that pick the order of a grid (Trending, New): one pressed at a time. */
function ListPills({
  lists,
  list,
  onChange,
}: {
  lists: PrivateList[]
  list: PrivateList
  onChange: (list: PrivateList) => void
}) {
  const { t } = useTranslation('member')
  return (
    <div role="group" aria-label={t('private.listsLabel')} className="flex gap-1">
      {lists.map((l) => (
        <button
          key={l}
          type="button"
          aria-pressed={l === list}
          onClick={() => onChange(l)}
          className={cn(
            'h-9 shrink-0 cursor-pointer rounded-full px-3.5 text-sm max-lg:h-11 font-medium whitespace-nowrap text-text-2 transition-colors duration-(--dur-1) hover:text-text',
            l === list ? 'bg-night-2 text-text' : 'hover:bg-night-1',
          )}
        >
          {t(`private.lists.${l}`)}
        </button>
      ))}
    </div>
  )
}

function dedupe(cards: PrivateCard[]): PrivateCard[] {
  const seen = new Set<string>()
  return cards.filter((c) => !seen.has(c.id) && Boolean(seen.add(c.id)))
}

/** "Show more scenes" under a grid that has another page. */
function MoreButton({ onClick, busy }: { onClick: () => void; busy: boolean }) {
  const { t } = useTranslation('member')
  return (
    <div className="mt-10 flex justify-center">
      <Button variant="ghost" disabled={busy} onClick={onClick}>
        {t('private.more')}
      </Button>
    </div>
  )
}

/** `/private`: the scenes of one list as a grid, Trending by default (W5). */
export function PrivateHomePage() {
  const { t } = useTranslation(['member', 'common'])
  const area = usePrivateArea()
  const [chosen, setList] = useUrlState('list', LIST, 'trending')
  const lists = [...(area.data?.lists ?? [])].sort(
    (a, b) => LIST.options.indexOf(a) - LIST.options.indexOf(b),
  )
  const list: PrivateList = lists.includes(chosen) ? chosen : (lists[0] ?? 'trending')
  if (!area.data) return null
  return (
    <>
      <AreaHeading description={t('private.description')}>{t('private.title')}</AreaHeading>
      {!area.data.ready ? (
        <EmptyState
          icon={HourglassIcon}
          title={t('private.notReadyTitle')}
          description={t('private.notReadyDescription')}
        />
      ) : (
        <section aria-labelledby="private-grid" className="flex flex-col gap-5">
          <div className="flex flex-wrap items-center gap-x-6 gap-y-3">
            <h2 id="private-grid" className="type-display text-2xl font-bold text-text">
              {t(`private.listTitles.${list}`)}
            </h2>
            <ListPills lists={lists} list={list} onChange={(l) => setList(l)} />
          </div>
          {!area.data.requestable && <RequestsClosed />}
          <ListGrid key={list} list={list} requestable={area.data.requestable} />
        </section>
      )}
    </>
  )
}

function ListGrid({ list, requestable }: { list: PrivateList; requestable: boolean }) {
  const { t } = useTranslation('member')
  const query = usePrivateLane(list, true)
  const selection = useSceneSelection()
  const cards = dedupe((query.data?.pages ?? []).flatMap((p) => p.items))
  if (query.isError && !query.data) {
    return <QueryError message={t('private.listError')} onRetry={() => void query.refetch()} />
  }
  if (query.isSuccess && cards.length === 0) {
    return (
      <p className="rounded-card border border-dashed border-line px-5 py-8 text-base text-text-2">
        {t('private.listEmpty')}
      </p>
    )
  }
  return (
    <>
      <SceneGrid
        cards={cards}
        label="private-grid"
        selection={requestable ? selection : undefined}
        loading={query.isPending ? 8 : query.isFetchingNextPage ? 4 : 0}
      />
      {query.hasNextPage && !query.isFetchingNextPage && (
        <MoreButton onClick={() => void query.fetchNextPage()} busy={query.isFetching} />
      )}
      {requestable && <SelectionBar selection={selection} grid="private-grid" />}
    </>
  )
}

/** The entity filter of the search page, from `?studio=` or `?performer=` (one at a time). */
function useSearchFilter(): SearchFilter | null {
  const [params] = useSearchParams()
  const uuid = z.string().uuid()
  for (const kind of ['studio', 'performer'] as const) {
    const id = params.get(kind)
    if (id && uuid.safeParse(id).success) return { kind, id }
  }
  return null
}

/** A compact lane of studios or performers on the search page; one tab stop. */
function EntityLane({
  id,
  kind,
  items,
  count,
  filter,
  followed,
  onPick,
}: {
  id: string
  kind: 'studio' | 'performer'
  items: PrivateCard[]
  count: number
  filter: SearchFilter | null
  followed: Set<string>
  onPick: (card: PrivateCard) => void
}) {
  const { t } = useTranslation('member')
  const { itemProps, onKeyDown } = useRovingFocus<HTMLButtonElement>(items.length)
  return (
    <section aria-labelledby={id} className="flex flex-col gap-3">
      <h2 id={id} className="type-display flex items-baseline gap-2.5 text-xl font-bold text-text">
        {t(`private.groups.${kind === 'studio' ? 'studios' : 'performers'}`)}{' '}
        <span className="type-data text-sm font-normal text-text-3">{formatNumber(count)}</span>
      </h2>
      <ul
        aria-labelledby={id}
        onKeyDown={onKeyDown}
        className="-mx-4 flex snap-x scroll-px-4 gap-5 overflow-x-auto px-4 pt-1.5 pb-3 [scrollbar-width:thin] sm:-mx-10 sm:scroll-px-10 sm:px-10"
      >
        {items.map((card, index) => {
          const pressed = filter?.id === card.id
          const following = followed.has(card.id)
          return (
            <li
              key={card.id}
              className={cn('shrink-0 snap-start', kind === 'studio' ? 'w-36 sm:w-40' : 'w-24')}
            >
              <button
                type="button"
                {...itemProps(index)}
                aria-pressed={pressed}
                aria-label={t('private.filterBy', { name: card.title })}
                aria-description={following ? t('private.following') : undefined}
                onClick={() => onPick(card)}
                className="group w-full cursor-pointer rounded-card focus-visible:outline-none"
              >
                <EntityTileBody
                  kind={kind}
                  image={card.image}
                  name={card.title}
                  following={following}
                  size="sm"
                />
              </button>
            </li>
          )
        })}
      </ul>
    </section>
  )
}

/** "Studio S ×": the entity filter as a removable chip; its name opens the entity page. */
function FilterChip({
  filter,
  name,
  onRemove,
}: {
  filter: SearchFilter
  name: string
  onRemove: () => void
}) {
  const { t } = useTranslation('member')
  return (
    <span className="inline-flex h-9 items-center gap-1 rounded-full border border-line bg-night-2 pr-1 pl-3.5 text-sm text-text">
      <Link
        to={entityPath(filter.kind, filter.id)}
        aria-label={t('private.openEntity', { name })}
        className="max-w-48 truncate rounded-control hover:underline"
      >
        {name}
      </Link>
      <button
        type="button"
        onClick={onRemove}
        aria-label={t('private.removeFilter', { name })}
        className="grid size-7 cursor-pointer place-items-center rounded-full text-text-2 hover:bg-night-1 hover:text-text"
      >
        <XIcon aria-hidden className="size-4" />
      </button>
    </span>
  )
}

function BackToBrowsing() {
  const { t } = useTranslation('member')
  return (
    <Button asChild variant="ghost">
      <Link to="/private">
        <ArrowLeftIcon aria-hidden />
        {t('private.backToBrowsing')}
      </Link>
    </Button>
  )
}

/** The ids the member follows, to mark tiles "Following". */
function useFollowed(): Set<string> {
  const follows = usePrivateFollows()
  return new Set(
    (follows.data ?? []).map((f: PrivateFollow) => f.entityId).filter((id): id is string => !!id),
  )
}

/**
 * `/private/search?q=&studio=|performer=`: studios first (a compact lane), performers next (only
 * with matches), then the scenes as a grid with counts (PRIV-14). Picking a studio or performer
 * narrows the grid and shows the filter chip (Q7 C). The text comes from the header search.
 */
export function PrivateSearchPage() {
  const { t } = useTranslation(['member', 'common'])
  const area = usePrivateArea()
  const navigate = useNavigate()
  const [params] = useSearchParams()
  const query = (params.get('q') ?? '').trim()
  const filter = useSearchFilter()
  const search = usePrivateSearch(query, filter)
  const entity = usePrivateEntityName(filter, search.data?.pages[0])
  const followed = useFollowed()
  const selection = useSceneSelection(`${query}|${filter?.kind ?? ''}|${filter?.id ?? ''}`)
  const errorMessage = useApiErrorMessage()
  const requestable = area.data?.requestable === true

  const setFilter = (next: SearchFilter | null) =>
    void navigate(searchPath(query, next), { replace: true })

  if (!query) {
    return (
      <>
        <AreaHeading>{t('private.searchTitle')}</AreaHeading>
        <EmptyState
          icon={SearchIcon}
          title={t('private.searchPromptTitle')}
          description={t('private.searchPromptDescription')}
          action={<BackToBrowsing />}
        />
      </>
    )
  }

  const first = search.data?.pages[0]
  const scenes = dedupe((search.data?.pages ?? []).flatMap((p) => p.scenes))
  const counts = first?.counts
  const nothing =
    first &&
    first.scenes.length === 0 &&
    first.studios.length === 0 &&
    first.performers.length === 0
  return (
    <>
      <AreaHeading>{t('private.resultsFor', { query })}</AreaHeading>
      {search.isError && !search.data ? (
        <QueryError
          message={`${t('search.error')} ${errorMessage(search.error)}`}
          onRetry={() => void search.refetch()}
        />
      ) : nothing && !filter ? (
        <EmptyState
          icon={SearchXIcon}
          title={t('private.noResultsTitle', { query })}
          description={t('private.noResultsDescription')}
          action={<BackToBrowsing />}
        />
      ) : (
        <div className="flex flex-col gap-10">
          {first && first.studios.length > 0 && (
            <EntityLane
              id="private-studios"
              kind="studio"
              items={first.studios}
              count={counts?.studios ?? first.studios.length}
              filter={filter}
              followed={followed}
              onPick={(card) =>
                setFilter(filter?.id === card.id ? null : { kind: 'studio', id: card.id })
              }
            />
          )}
          {first && first.performers.length > 0 && (
            <EntityLane
              id="private-performers"
              kind="performer"
              items={first.performers}
              count={counts?.performers ?? first.performers.length}
              filter={filter}
              followed={followed}
              onPick={(card) =>
                setFilter(filter?.id === card.id ? null : { kind: 'performer', id: card.id })
              }
            />
          )}
          <section aria-labelledby="private-scenes" className="flex flex-col gap-5">
            <div className="flex flex-wrap items-center gap-x-5 gap-y-3">
              <h2
                id="private-scenes"
                className="type-display flex items-baseline gap-2.5 text-2xl font-bold text-text"
              >
                {t('private.groups.scenes')}{' '}
                {counts && (
                  <span className="type-data text-sm font-normal text-text-3">
                    {formatNumber(counts.scenes)}
                  </span>
                )}
              </h2>
              {filter && (
                <FilterChip
                  filter={filter}
                  name={entity ?? t(`private.kinds.${filter.kind}`)}
                  onRemove={() => setFilter(null)}
                />
              )}
            </div>
            {!requestable && <RequestsClosed />}
            {first && scenes.length === 0 ? (
              <div className="flex flex-col items-start gap-4 rounded-card border border-dashed border-line px-5 py-8">
                <p className="text-base text-text-2">
                  {filter
                    ? t('private.noScenesOf', { name: entity ?? '', query })
                    : t('private.noScenesFor', { query })}
                </p>
                {filter && (
                  <Button variant="ghost" size="sm" onClick={() => setFilter(null)}>
                    {t('private.removeFilterAction')}
                  </Button>
                )}
              </div>
            ) : (
              <SceneGrid
                cards={scenes}
                label="private-scenes"
                selection={requestable ? selection : undefined}
                loading={search.isPending ? 8 : search.isFetchingNextPage ? 4 : 0}
              />
            )}
            {search.hasNextPage && !search.isFetchingNextPage && (
              <MoreButton onClick={() => void search.fetchNextPage()} busy={search.isFetching} />
            )}
          </section>
          {requestable && <SelectionBar selection={selection} grid="private-scenes" />}
        </div>
      )}
    </>
  )
}

/**
 * The name of the filtered studio or performer: from the search's lanes when it is there, else
 * from its page (a deep link to a filtered search).
 */
function usePrivateEntityName(
  filter: SearchFilter | null,
  first: { studios: PrivateCard[]; performers: PrivateCard[] } | undefined,
): string | null {
  const inLane = filter
    ? (filter.kind === 'studio' ? first?.studios : first?.performers)?.find(
        (c) => c.id === filter.id,
      )?.title
    : undefined
  const page = usePrivateEntity(filter?.kind ?? 'studio', filter?.id ?? '', {
    enabled: filter !== null && first !== undefined && inLane === undefined,
  })
  return inLane ?? page.data?.name ?? null
}

/** "Back" (PRIV-13): one step back in the history, else the area's start. */
function Back() {
  const { t } = useTranslation('member')
  const { href, back } = usePrivateBack()
  return (
    <Link
      to={href}
      onClick={(event) => {
        if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return
        event.preventDefault()
        back()
      }}
      className="mb-6 inline-flex min-h-6 items-center gap-2 rounded-control text-sm text-text-2 hover:text-text max-lg:min-h-11"
    >
      <ArrowLeftIcon aria-hidden className="size-4" />
      {t('private.back')}
    </Link>
  )
}

function Detail({
  query,
  skeleton = <PrivatePageSkeleton />,
  children,
}: {
  query: { isPending: boolean; isError: boolean; error: unknown; refetch: () => unknown }
  skeleton?: ReactNode
  children: () => ReactNode
}) {
  const errorMessage = useApiErrorMessage()
  if (query.isPending) return skeleton
  if (query.isError) {
    return isNotFound(query.error) ? (
      <NotFound />
    ) : (
      <QueryError message={errorMessage(query.error)} onRetry={() => void query.refetch()} />
    )
  }
  return <>{children()}</>
}

/**
 * `/private/scenes/:id`: the watch layout (spec 0008 Q8 A) with the request card, the
 * description and "More from <studio>". Keyed by the scene, so going to another scene from the
 * list starts it fresh (entrance, collapsed description).
 */
export function PrivateScenePage() {
  const { id = '' } = useParams()
  const scene = usePrivateScene(id)
  const image = scene.data?.image
  return (
    // The ambient layer (A1) reaches up behind the header, as on Discover.
    <div className="relative isolate">
      {scene.data && (
        <Ambient
          src={image ? privateImage(image, '300') : null}
          className="-inset-x-4 -top-20 h-[64rem] [mask-image:linear-gradient(#000_45%,transparent)] [mask-repeat:no-repeat] sm:-inset-x-10 lg:-top-[140px]"
        />
      )}
      <Back />
      <Detail query={scene} skeleton={<SceneSkeleton />}>
        {() => scene.data && <SceneView key={id} scene={scene.data} />}
      </Detail>
    </div>
  )
}

/**
 * `/private/performers/:id` and `/private/studios/:id`: the entity in its own shape (round
 * performer, studio logo tile, PRIV-22) with the follow card, then its scenes newest first as
 * the grid; not-yet-requested scenes can be selected and requested together (at most 20). A
 * studio's tiles name the performers instead of repeating the studio (PRIV-25).
 */
export function PrivateEntityPage({ kind }: { kind: 'performer' | 'studio' }) {
  const { t } = useTranslation('member')
  const { id = '' } = useParams()
  const entity = usePrivateEntity(kind, id)
  const selection = useSceneSelection(`${kind}|${id}`)
  const mode: CaptionMode = kind === 'studio' ? 'performers' : 'studio-performers'
  return (
    <>
      <Back />
      <Detail query={entity}>
        {() =>
          entity.data && (
            <div className="flex flex-col gap-8">
              <div
                className={cn(
                  'flex flex-col gap-5 sm:flex-row sm:items-center sm:gap-7',
                  kind === 'performer' && 'items-start',
                )}
              >
                <div className={kind === 'performer' ? 'w-32 sm:w-40' : 'w-full max-w-72'}>
                  <EntityShape kind={kind} image={entity.data.image} name={entity.data.name} />
                </div>
                <div className="flex flex-col gap-2">
                  <p className="type-data flex items-center gap-2 text-xs text-text-3 uppercase">
                    <CategoryDot category="adult" className="size-2" />
                    {t(`private.kinds.${kind}`)}
                  </p>
                  <h1 className="type-display text-3xl font-extrabold text-text sm:text-4xl">
                    {entity.data.name}
                  </h1>
                  {entity.data.follow && <FollowingChip className="self-start" />}
                </div>
              </div>
              <PrivateFollowControl
                kind={kind}
                id={entity.data.id}
                follow={entity.data.follow}
                canFollow={entity.data.requestable}
              />
              {!entity.data.requestable && <RequestsClosed />}
              <section aria-labelledby="private-scenes" className="flex flex-col gap-4">
                <div className="flex flex-wrap items-baseline justify-between gap-x-6 gap-y-1">
                  <h2
                    id="private-scenes"
                    className="type-display flex items-baseline gap-2.5 text-xl font-bold text-text"
                  >
                    {t('private.groups.scenes')}{' '}
                    <span className="type-data text-sm font-normal text-text-3">
                      {formatNumber(entity.data.scenes.length)}
                    </span>
                  </h2>
                  {entity.data.requestable && entity.data.scenes.length > 0 && (
                    <p className="text-sm text-text-3">
                      {t('private.selectionHint', { max: MAX_SCENES })}
                    </p>
                  )}
                </div>
                {entity.data.scenes.length === 0 ? (
                  <div className="flex flex-col items-start gap-4 rounded-card border border-dashed border-line px-5 py-8">
                    <p className="text-base text-text-2">{t(`private.noScenes_${kind}`)}</p>
                    <Button asChild variant="ghost" size="sm">
                      <Link to="/private">
                        {t('private.backToBrowsing')}
                        <ArrowRightIcon aria-hidden />
                      </Link>
                    </Button>
                  </div>
                ) : (
                  <SceneGrid
                    cards={entity.data.scenes}
                    label="private-scenes"
                    mode={mode}
                    selection={entity.data.requestable ? selection : undefined}
                  />
                )}
              </section>
              {entity.data.requestable && (
                <SelectionBar selection={selection} grid="private-scenes" />
              )}
            </div>
          )
        }
      </Detail>
    </>
  )
}
