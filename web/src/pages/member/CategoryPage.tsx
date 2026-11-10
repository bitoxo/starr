import { useCallback, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { z } from 'zod'

import { tmdbImage } from '@/api/categories'
import {
  type DiscoverCategory,
  type Highlight,
  type TitleCard,
  uniqueCards,
  useArtist,
  useDiscoverCategories,
  useDiscoverList,
  useHighlights,
} from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { useFollows } from '@/api/follows'
import { Ambient } from '@/components/Ambient'
import { CategoryDot } from '@/components/CategoryDot'
import { GenreTile } from '@/components/GenreTile'
import { Hero } from '@/components/Hero'
import { ErrorNotice } from '@/components/members'
import { ArtistAvatar, ProviderLogo, ProviderSwitcher } from '@/components/ProviderSwitcher'
import { Skeleton } from '@/components/Skeleton'
import { StateChip } from '@/components/StateChip'
import { CoverLane, type LaneLook, SeeAllLink } from '@/components/Swimlane'
import { TitleFacts } from '@/components/TitleFacts'
import { Button } from '@/components/ui/button'
import { isBrowseCategory } from '@/components/shell/nav'
import { fixedSource, listSource } from '@/lib/laneSource'
import { afterHero, discographyCards, splitFresh, todayIso } from '@/lib/categoryPage'
import { cardState } from '@/lib/states'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { useUrlState } from '@/lib/urlState'
import { cn } from '@/lib/utils'
import { NotFound } from '@/pages/ErrorPage'

/** Genre tiles shown before "Show all genres". */
const TILES = 8

/** The page's shape while the categories load. */
function PageSkeleton() {
  const { t } = useTranslation('common')
  return (
    <div aria-busy className="flex flex-col gap-6">
      <p role="status" className="sr-only">
        {t('status.loading')}
      </p>
      <Skeleton className="h-10 w-48" />
      <div className="flex gap-4 overflow-hidden">
        {Array.from({ length: 7 }, (_, i) => (
          <Skeleton key={i} className="aspect-2/3 w-36 shrink-0 rounded-card sm:w-44" />
        ))}
      </div>
    </div>
  )
}

/**
 * `/discover/:category` (spec 0008 W4, Q6 A): the curated page of one category the member may
 * browse; any other category is "not found", the same as every unknown URL.
 */
export default function CategoryPage() {
  const { category = '' } = useParams()
  const categories = useDiscoverCategories()
  const errorMessage = useApiErrorMessage()
  const entry = categories.data?.find((c) => c.category === category && c.discoverReady)

  if (categories.isError) return <ErrorNotice>{errorMessage(categories.error)}</ErrorNotice>
  if (categories.isPending) return <PageSkeleton />
  if (!isBrowseCategory(category) || !entry) return <NotFound />
  return <CategoryView key={category} category={entry} />
}

/** One section of the page: heading, an optional control beside it, "See all", then content. */
function Section({
  id,
  title,
  control,
  seeAll,
  children,
}: {
  id: string
  title: string
  control?: ReactNode
  /** The section's grid and the lane's name for the link. */
  seeAll?: { to: string; lane: string }
  children: ReactNode
}) {
  return (
    <section id={id} aria-labelledby={`${id}-h`} className="flex scroll-mt-24 flex-col gap-3">
      <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
        <h2 id={`${id}-h`} className="type-display text-xl font-extrabold text-text sm:text-2xl">
          {title}
        </h2>
        {control && <div className="flex min-w-0 max-sm:basis-full">{control}</div>}
        {seeAll && <SeeAllLink to={seeAll.to} lane={seeAll.lane} className="ml-auto" />}
      </div>
      {children}
    </section>
  )
}

/**
 * The curated sections of a category (Q6 A): for movies and series the category's own hero
 * with its top 10, "More trending" with ranks 11–20 (F-3), "On \<provider\>" with the provider
 * switcher and a spotlight (Q13 B, F-7), genre tiles in their covers' tint (A1), then Top rated,
 * "New episodes this week" (series) and Upcoming with exact days; for music the trending top 10
 * with ranks, "From artists you follow" with an artist switcher, a lane per genre, genre tiles,
 * fresh releases, "Out soon" with exact days, and the MusicBrainz credit (Q17 B).
 */
function CategoryView({ category }: { category: DiscoverCategory }) {
  const { t } = useTranslation(['member', 'common'])
  const { open } = useTitleSheet()
  const tmdb = category.kind === 'movie' || category.kind === 'series'
  const name = t(`common:categories.${category.category}`)
  const highlights = useHighlights(category.category, { enabled: tmdb })
  const hero = tmdb && !highlights.isError && (highlights.data?.length ?? 1) > 0
  const [ambient, setAmbient] = useState<string | null>(null)
  const onActive = useCallback(
    (item: Highlight | null) =>
      setAmbient(item?.backdrop ? tmdbImage(item.backdrop, 'w300') : null),
    [],
  )
  const has = (list: string) => (category.lists as string[]).includes(list)
  const lane = (title: string) => t('category.lane', { category: name, lane: title })
  const props = {
    category,
    square: category.kind === 'album',
    requestable: category.requestable,
    onOpen: open,
  }
  const base = `/discover/${category.category}`

  // The ranked lane (F-3), shared with its lane through the query cache: left out when the
  // hero already shows every trending title.
  const trending = useDiscoverList(category.category, 'trending', {}, { enabled: has('trending') })
  const heroPending = tmdb && highlights.isPending
  const ranked = afterHero(
    trending.data?.pages[0]?.items ?? [],
    tmdb ? (highlights.data ?? []) : [],
  )
  const [provider, setProvider] = useChosenProvider(category)

  const sections: { id: string; title: string; node: ReactNode }[] = []
  if (has('trending') && !(trending.isSuccess && !heroPending && ranked.length === 0)) {
    const title = hero ? t('category.moreTrending') : t('category.trending')
    sections.push({
      id: 'trending',
      title,
      node: (
        <Section id="trending" title={title} seeAll={{ to: `${base}/trending`, lane: lane(title) }}>
          <CoverLane
            square={props.square}
            requestable={props.requestable}
            onOpen={open}
            label={lane(title)}
            look="ranked"
            ranks={ranked.map((r) => r.rank)}
            source={fixedSource(
              ranked.map((r) => r.card),
              {
                // Waits for the hero, so the ranks do not jump.
                isPending: trending.isPending || heroPending,
                isError: trending.isError && !trending.data,
                isFetching: trending.isFetching,
                refetch: () => void trending.refetch(),
              },
            )}
          />
        </Section>
      ),
    })
  }
  if (tmdb && provider) {
    sections.push({
      id: 'provider',
      title: t('category.onProvider', { provider: provider.name }),
      node: <ProviderSection {...props} provider={provider} onChoose={setProvider} />,
    })
  }
  if (!tmdb) {
    sections.push({
      id: 'artists',
      title: t('category.followedArtists'),
      node: <FollowedArtists {...props} label={lane(t('category.followedArtists'))} />,
    })
    if (category.genres.length > 0) {
      sections.push({
        id: 'by-genre',
        title: t('category.byGenre'),
        node: <GenreLane {...props} />,
      })
    }
  }
  // TMDB's "On the air" (series only) is a category page list, not one of Discover's.
  if (category.kind === 'series') {
    const title = t('category.onTheAir')
    sections.push({
      id: 'on-the-air',
      title,
      node: (
        <Section
          id="on-the-air"
          title={title}
          seeAll={{ to: `${base}/on_the_air`, lane: lane(title) }}
        >
          <ListLane {...props} list="on_the_air" label={lane(title)} />
        </Section>
      ),
    })
  }
  if (category.genres.length > 0) {
    sections.push({
      id: 'genres',
      title: t('category.genres'),
      node: <GenreTiles category={category} />,
    })
  }
  if (has('top_rated')) {
    const title = t('discover.lists.top_rated')
    sections.push({
      id: 'top-rated',
      title,
      node: (
        <Section
          id="top-rated"
          title={title}
          seeAll={{ to: `${base}/top_rated`, lane: lane(title) }}
        >
          <ListLane {...props} list="top_rated" label={lane(title)} />
        </Section>
      ),
    })
  }
  if (has('fresh_releases')) {
    sections.push({
      id: 'fresh',
      title: t('discover.lists.fresh_releases'),
      node: <FreshReleases {...props} lane={lane} />,
    })
  }
  if (!tmdb && has('popular')) {
    const title = t('discover.lists.popular')
    sections.push({
      id: 'popular',
      title,
      node: (
        <Section id="popular" title={title} seeAll={{ to: `${base}/popular`, lane: lane(title) }}>
          <ListLane {...props} list="popular" label={lane(title)} />
        </Section>
      ),
    })
  }
  if (tmdb && has('upcoming')) {
    const title = t('discover.lists.upcoming')
    sections.push({
      id: 'upcoming',
      title,
      node: (
        <Section id="upcoming" title={title} seeAll={{ to: `${base}/upcoming`, lane: lane(title) }}>
          <ListLane {...props} list="upcoming" label={lane(title)} look="dated" />
        </Section>
      ),
    })
  }

  return (
    <div className="relative isolate">
      <Ambient
        src={ambient}
        className="-inset-x-4 -top-8 h-[64rem] [mask-image:linear-gradient(#000_45%,transparent)] [mask-repeat:no-repeat] sm:-inset-x-10 lg:-top-[140px]"
      />
      {hero ? (
        <h1 className="sr-only">{name}</h1>
      ) : (
        <header className="flex flex-col gap-2">
          <h1 className="type-display flex items-center gap-3 text-3xl font-extrabold text-text sm:text-4xl">
            <CategoryDot category={category.category} className="size-3" />
            {name}
          </h1>
          <p className="max-w-[60ch] text-base text-text-2">
            {t(`category.tagline.${category.category as 'movies' | 'series' | 'music'}`)}
          </p>
        </header>
      )}
      {/* One hero in one place: it shows its stage, its skeleton or its error with a retry
          (and nothing without titles); remounting it would refetch a failed top 10. */}
      {tmdb && <Hero category={category.category} onOpen={open} onActiveChange={onActive} />}
      <nav
        aria-label={t('category.onThisPage')}
        className={cn('flex flex-wrap gap-1.5', hero ? 'mt-8 lg:mt-10' : 'mt-5')}
      >
        {sections.map((s) => (
          <a
            key={s.id}
            href={`#${s.id}`}
            className="inline-flex h-8 items-center rounded-full px-3.5 text-sm text-text-2 ring-1 ring-line transition-colors duration-(--dur-1) ring-inset hover:text-text max-lg:h-11 max-lg:px-4"
          >
            {s.title}
          </a>
        ))}
      </nav>
      {!category.requestable && (
        <p className="mt-4 text-sm text-text-3">{t('discover.requestsClosed')}</p>
      )}
      <div className="mt-10 flex flex-col gap-12">
        {sections.map((s) => (
          <div key={s.id}>{s.node}</div>
        ))}
      </div>
      <Credits category={category} />
    </div>
  )
}

type LaneProps = {
  category: DiscoverCategory
  square: boolean
  requestable: boolean
  onOpen: (card: TitleCard) => void
}

/** One catalog list of the category as a lane. */
function ListLane({
  category,
  list,
  label,
  look,
  ...rest
}: LaneProps & {
  list: 'top_rated' | 'upcoming' | 'popular' | 'on_the_air'
  label: string
  look?: LaneLook
}) {
  const query = useDiscoverList(category.category, list)
  return <CoverLane {...rest} label={label} look={look} source={listSource(query)} />
}

type Provider = DiscoverCategory['providers'][number]

/**
 * The provider of "On \<provider\>": `?provider=` (F6) when it is in the switcher, else the
 * first one; none for a category without providers.
 */
function useChosenProvider(
  category: DiscoverCategory,
): [Provider | undefined, (id: number) => void] {
  const providers = category.providers
  const ids = providers.map((p) => p.id)
  const [chosen, setChosen] = useUrlState<number>(
    'provider',
    z.coerce.number().refine((id) => ids.includes(id)),
    ids[0] ?? 0,
  )
  return [providers.find((p) => p.id === chosen) ?? providers[0], setChosen]
}

/**
 * "On \<provider\>" (Q6 A, Q13 B, F-7): the switcher chooses the provider, the list's first
 * title with a backdrop is the spotlight, the rest the lane (left out when nothing is left).
 * Labelled "On" because TMDB cannot prove exclusivity.
 */
function ProviderSection({
  category,
  provider,
  onChoose,
  ...rest
}: LaneProps & { provider: Provider; onChoose: (id: number) => void }) {
  const { t } = useTranslation(['member', 'common'])
  const providers = category.providers

  const query = useDiscoverList(category.category, 'on_provider', { provider: provider.id })
  const cards = uniqueCards(query.data?.pages)
  const spotlight = cards.find((c) => c.backdrop)
  const others = cards.filter((c) => c !== spotlight)
  const title = t('category.onProvider', { provider: provider.name })
  const name = t(`common:categories.${category.category}`)
  const label = t('category.lane', { category: name, lane: title })
  return (
    <Section
      id="provider"
      title={title}
      control={
        <ProviderSwitcher
          label={t('category.providers')}
          panel="provider-panel"
          chosen={String(provider.id)}
          onChoose={(id) => onChoose(Number(id))}
          options={providers.map((p) => ({
            id: String(p.id),
            name: p.name,
            image: <ProviderLogo logo={p.logo} name={p.name} />,
          }))}
        />
      }
      seeAll={{
        to: `/discover/${category.category}/on_provider?provider=${provider.id}`,
        lane: label,
      }}
    >
      <div id="provider-panel" role="tabpanel" aria-label={title} className="flex flex-col gap-4">
        {spotlight && (
          <Spotlight
            key={`${spotlight.kind}-${spotlight.id}`}
            card={spotlight}
            provider={provider}
            onOpen={rest.onOpen}
          />
        )}
        {!(spotlight && others.length === 0 && !query.hasNextPage) && (
          <CoverLane
            key={provider.id}
            {...rest}
            label={label}
            source={{ ...listSource(query), cards: others }}
          />
        )}
      </div>
    </Section>
  )
}

/**
 * The provider's spotlight: its most popular title with a backdrop, large, with the provider,
 * facts, synopsis, the state chip and Details. The page's gold stays with the hero; requesting
 * goes through the detail view, the info strip or the preview card.
 */
function Spotlight({
  card,
  provider,
  onOpen,
}: {
  card: TitleCard
  provider: DiscoverCategory['providers'][number]
  onOpen: (card: TitleCard) => void
}) {
  const { t } = useTranslation('member')
  const state = cardState(card)
  return (
    <div
      data-spotlight
      className="relative isolate flex min-h-64 items-end overflow-hidden rounded-panel bg-night-1 sm:min-h-72"
    >
      {card.backdrop && (
        <img
          src={tmdbImage(card.backdrop, 'w1280')}
          srcSet={`${tmdbImage(card.backdrop, 'w780')} 780w, ${tmdbImage(card.backdrop, 'w1280')} 1280w`}
          sizes="(min-width: 1024px) 1100px, 100vw"
          alt=""
          loading="lazy"
          decoding="async"
          className="absolute inset-0 -z-10 size-full object-cover object-[center_25%]"
        />
      )}
      <div aria-hidden className="hero-scrim absolute inset-0 -z-10" />
      <div className="flex max-w-xl flex-col items-start gap-2 p-5 sm:p-8">
        <p className="flex items-center gap-2 text-sm font-medium text-text-2">
          <ProviderLogo logo={provider.logo} name={provider.name} className="size-5" />
          {t('category.onProvider', { provider: provider.name })}
        </p>
        <h3
          title={card.title}
          className="type-display line-clamp-2 text-2xl font-extrabold text-balance text-text sm:text-3xl"
        >
          {card.title}
        </h3>
        <TitleFacts card={card} />
        {card.overview && (
          <p className="line-clamp-2 max-w-[52ch] text-base text-text">{card.overview}</p>
        )}
        <div className="mt-2 flex flex-wrap items-center gap-2.5">
          {state && <StateChip state={state} />}
          <Button
            variant="secondary"
            className="max-lg:h-11"
            aria-label={t('discover.card.detailsOf', { title: card.title })}
            onClick={() => onOpen(card)}
          >
            {t('discover.card.details')}
          </Button>
        </div>
      </div>
    </div>
  )
}

/** Genre tiles (A6, A1): the first eight, the rest behind "Show all genres". */
function GenreTiles({ category }: { category: DiscoverCategory }) {
  const { t } = useTranslation('member')
  const [all, setAll] = useState(false)
  const shown = all ? category.genres : category.genres.slice(0, TILES)
  return (
    <Section id="genres" title={t('category.genres')}>
      <ul id="genre-tiles" className="grid grid-cols-2 gap-3 sm:gap-4 lg:grid-cols-4">
        {shown.map((genre) => (
          <li key={genre.id}>
            <GenreTile
              category={category}
              genre={genre}
              to={`/discover/${category.category}/genre?genre=${encodeURIComponent(genre.id)}`}
            />
          </li>
        ))}
      </ul>
      {category.genres.length > TILES && (
        <Button
          variant="ghost"
          size="sm"
          aria-expanded={all}
          aria-controls="genre-tiles"
          className="self-start max-lg:h-11"
          onClick={() => setAll(!all)}
        >
          {all ? t('category.fewerGenres') : t('category.allGenres')}
        </Button>
      )}
    </Section>
  )
}

/**
 * Music: one lane per MusicBrainz genre (Q17 B, F-8), ranked by listens on the server; the
 * switcher chooses the genre (`?genre=`).
 */
function GenreLane({ category, ...rest }: LaneProps) {
  const { t } = useTranslation(['member', 'common'])
  const ids = category.genres.map((g) => g.id)
  const [chosen, setChosen] = useUrlState<string>(
    'genre',
    z.string().refine((g) => ids.includes(g)),
    ids[0] ?? '',
  )
  const genre = category.genres.find((g) => g.id === chosen) ?? category.genres[0]
  const query = useDiscoverList(category.category, 'genre', { genre: genre?.id })
  if (!genre) return null
  const name = t(`common:categories.${category.category}`)
  const label = t('category.lane', { category: name, lane: genre.name })
  return (
    <Section
      id="by-genre"
      title={t('category.byGenre')}
      control={
        <ProviderSwitcher
          label={t('category.genre')}
          panel="genre-panel"
          chosen={genre.id}
          onChoose={setChosen}
          options={category.genres.map((g) => ({ id: g.id, name: g.name }))}
        />
      }
      seeAll={{
        to: `/discover/${category.category}/genre?genre=${encodeURIComponent(genre.id)}`,
        lane: label,
      }}
    >
      <div id="genre-panel" role="tabpanel" aria-label={label}>
        <CoverLane key={genre.id} {...rest} label={label} source={listSource(query)} />
      </div>
    </Section>
  )
}

/**
 * Music: "From artists you follow" (Q6 A): the switcher chooses one of the followed artists
 * (`?artist=`), the lane shows their albums, newest first, with their state.
 */
function FollowedArtists({ category, label, ...rest }: LaneProps & { label: string }) {
  const { t } = useTranslation('member')
  const follows = useFollows()
  const artists = (follows.data ?? []).filter(
    (f) => f.kind === 'artist' && f.category === category.category && f.titleId,
  )
  const ids = artists.map((a) => a.titleId ?? '')
  const [chosen, setChosen] = useUrlState<string>(
    'artist',
    z.string().refine((id) => ids.includes(id)),
    ids[0] ?? '',
  )
  const current = ids.includes(chosen) ? chosen : (ids[0] ?? null)
  const artist = useArtist(current)
  const cards = artist.data ? discographyCards(artist.data) : []
  const pending = follows.isPending || (current !== null && artist.isPending)
  return (
    <Section
      id="artists"
      title={t('category.followedArtists')}
      control={
        artists.length > 0 && (
          <ProviderSwitcher
            label={t('category.artist')}
            panel="artists-panel"
            chosen={current ?? ''}
            onChoose={setChosen}
            options={artists.map((a) => ({
              id: a.titleId ?? '',
              name: a.title,
              image: <ArtistAvatar cover={a.poster} name={a.title} />,
            }))}
          />
        )
      }
    >
      {follows.isSuccess && artists.length === 0 ? (
        <p className="rounded-card border border-dashed border-line px-5 py-8 text-base text-text-2">
          {t('category.noFollowedArtists')}
        </p>
      ) : (
        <div id="artists-panel" role="tabpanel" aria-label={label}>
          <CoverLane
            key={current ?? ''}
            {...rest}
            label={label}
            source={fixedSource(cards, {
              isPending: pending,
              isError: (follows.isError && !follows.data) || artist.isError,
              refetch: () => void (follows.isError ? follows.refetch() : artist.refetch()),
            })}
          />
        </div>
      )}
    </Section>
  )
}

/**
 * Music: fresh releases (released by today) and "Out soon" (after today, soonest first, with
 * the exact day), both from ListenBrainz's fresh releases. "Out soon" is left out while it has
 * nothing.
 */
function FreshReleases({
  category,
  lane,
  ...rest
}: LaneProps & { lane: (title: string) => string }) {
  const { t } = useTranslation('member')
  const query = useDiscoverList(category.category, 'fresh_releases')
  const source = listSource(query)
  const { outSoon, fresh } = splitFresh(source.cards, todayIso())
  const freshTitle = t('discover.lists.fresh_releases')
  const soonTitle = t('category.outSoon')
  return (
    <div className="flex flex-col gap-12">
      <Section
        id="fresh"
        title={freshTitle}
        seeAll={{ to: `/discover/${category.category}/fresh_releases`, lane: lane(freshTitle) }}
      >
        <CoverLane {...rest} label={lane(freshTitle)} source={{ ...source, cards: fresh }} />
      </Section>
      {outSoon.length > 0 && (
        <Section id="out-soon" title={soonTitle}>
          <CoverLane
            {...rest}
            label={lane(soonTitle)}
            look="dated"
            source={fixedSource(outSoon, {
              isPending: false,
              isError: false,
              refetch: source.refetch,
            })}
          />
        </Section>
      )}
    </div>
  )
}

/** Credits for what the page shows: JustWatch for providers, MusicBrainz genres (Q17 B). */
function Credits({ category }: { category: DiscoverCategory }) {
  const { t } = useTranslation('member')
  if (category.attributions.length === 0) return null
  return (
    <footer className="mt-14 flex flex-wrap gap-x-6 gap-y-1 border-t border-line pt-5 text-xs text-text-3">
      {category.attributions.map((a) => (
        <a
          key={a.data}
          href={a.url}
          target="_blank"
          rel="noreferrer"
          className="rounded-control underline-offset-2 hover:text-text-2 hover:underline"
        >
          {a.data === 'genres'
            ? t('category.credit.genres', { source: a.source })
            : t('category.credit.providers', { source: a.source })}
        </a>
      ))}
    </footer>
  )
}
