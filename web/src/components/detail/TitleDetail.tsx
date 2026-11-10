import { CalendarIcon, CircleCheckIcon, ChevronDownIcon, ClockIcon, StarIcon } from 'lucide-react'
import { useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { toast } from 'sonner'

import { tmdbImage } from '@/api/categories'
import {
  type CreatedRequest,
  type SeasonDetails,
  type TitleDetails,
  type TitleRef,
  useCreateRequest,
  useSeasonEpisodes,
  useTitle,
} from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { DetailLoad, MorphTarget } from '@/components/detail/states'
import { useDetailAmbient, useDetailDocumentTitle } from '@/lib/detailStage'
import {
  CategoryLine,
  CertificationBadge,
  Credit,
  DetailBody,
  DetailHeader,
  DetailTabs,
  DetailTitle,
  Facts,
  FactList,
  FollowSwitch,
  Monogram,
  RequestCard,
  SectionHeading,
  SelectionBar,
  TrailerLink,
} from '@/components/detail/parts'
import { Cover } from '@/components/Poster'
import { AvailabilityChip, RequestStateChip } from '@/components/RequestStatus'
import { JourneyStatusChip } from '@/components/StatusJourney'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { crewGroups, isNotFound, providerGroups, seriesEnded } from '@/lib/detail'
import {
  formatDay,
  formatLanguage,
  formatMinutes,
  formatNumber,
  formatUpcomingDay,
} from '@/lib/format'
import { useJourneyTitle } from '@/lib/journey'
import { focusElement } from '@/lib/returnFocus'
import { seasonBlock, selectableSeasons, toggleAll } from '@/lib/seasons'
import { cardState, requestState } from '@/lib/states'
import { cn } from '@/lib/utils'

/** A movie or series in the detail overlay (spec 0008 A7, prototype 02-B). */
export function TitleDetail({ titleRef }: { titleRef: TitleRef }) {
  const details = useTitle(titleRef)
  if (!details.data) {
    return (
      <DetailLoad
        kind={titleRef.kind}
        error={details.error}
        onRetry={() => void details.refetch()}
      />
    )
  }
  return <TitleView details={details.data} />
}

function TitleView({ details }: { details: TitleDetails }) {
  const { t } = useTranslation(['member', 'common'])
  const journeyTitle = useJourneyTitle()
  const isSeries = details.kind === 'series'
  const [tab, setTab] = useState('overview')
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [outcome, setOutcome] = useState<CreatedRequest | null>(null)
  const card = useRef<HTMLElement>(null)
  const create = useCreateRequest()
  const errorMessage = useApiErrorMessage()
  useDetailAmbient(details.backdrop ? tmdbImage(details.backdrop, 'w300') : null)
  useDetailDocumentTitle(details.title)

  // Downloaded already but not in a journey of the member's: say how close it is.
  const state =
    details.journeys.length === 0 && details.status === 'requested' && details.stage
      ? requestState('in_progress', details.stage, details.arrivingIn)
      : cardState(details)
  const anyOpen =
    isSeries && details.requestable && details.seasons.some((s) => seasonBlock(s) === null)
  const canRequestMovie =
    !isSeries && details.requestable && details.status !== 'available' && !details.requestedByMe
  // One gold per view: Request while it can be asked for, else "Open in".
  const openInPrimary = !canRequestMovie && !(isSeries && selected.size > 0)

  const submit = (body: Parameters<typeof create.mutate>[0], focusAfter: HTMLElement | null) =>
    create.mutate(body, {
      onSuccess: (done) => {
        setOutcome(done)
        setSelected(new Set())
        const pending = done.request.state === 'pending_approval'
        toast(
          t(pending ? 'discover.card.requestedPending' : 'discover.card.requestedApproved', {
            title: details.title,
          }),
        )
        // The button gives way: focus stays in the card, never on <body> (F3).
        if (focusAfter?.isConnected) focusElement(focusAfter, { quiet: true })
      },
      onError: (error) => toast.error(errorMessage(error)),
    })

  const note = !details.requestable
    ? t('title.notRequestable')
    : details.status === 'available' && details.openIn.length === 0
      ? t('title.isAvailable')
      : details.status === 'requested' && !details.requestedByMe && details.journeys.length === 0
        ? t('title.isRequestedByOthers')
        : isSeries && selected.size > 0
          ? t('detail.seasonsChosen', { count: selected.size })
          : isSeries && anyOpen
            ? t('detail.chooseSeasonsHint')
            : canRequestMovie
              ? t('detail.requestHint')
              : null

  const { leads, writers } = crewGroups(details.crew)
  const ended = seriesEnded(details.seriesStatus)

  const tabs = [
    {
      value: 'overview',
      label: t('detail.tabs.overview'),
      content: <Overview details={details} />,
    },
    ...(isSeries
      ? [
          {
            value: 'seasons',
            label: t('detail.tabs.seasons'),
            keepMounted: true,
            content: (
              <Seasons
                details={details}
                selected={selected}
                onSelect={setSelected}
                pending={create.isPending}
                onRequest={(focusAfter) =>
                  submit(
                    {
                      kind: 'series',
                      titleId: details.id,
                      seasons: [...selected].sort((a, b) => a - b),
                    },
                    focusAfter,
                  )
                }
              />
            ),
          },
        ]
      : []),
    { value: 'cast', label: t('detail.tabs.cast'), content: <Cast details={details} /> },
    {
      value: 'details',
      label: t('detail.tabs.details'),
      content: <Details details={details} writers={writers} leads={leads} />,
    },
  ]

  return (
    <article>
      <DetailHeader
        backdrop={
          details.backdrop
            ? {
                src: tmdbImage(details.backdrop, 'w1280'),
                srcSet: `${tmdbImage(details.backdrop, 'w780')} 780w, ${tmdbImage(details.backdrop, 'w1280')} 1280w`,
              }
            : null
        }
        cover={
          <MorphTarget>
            <Cover
              file={details.poster}
              title={details.title}
              kind={details.kind}
              sizes="(min-width: 640px) 152px, 96px"
              className="shadow-[0_12px_28px_rgb(0_0_0/0.45)] ring-1 ring-text/10"
            />
          </MorphTarget>
        }
      >
        <CategoryLine
          category={details.category}
          extra={isSeries ? details.networks.slice(0, 2).join(', ') : undefined}
        />
        <DetailTitle>{details.title}</DetailTitle>
        {details.tagline && <p className="text-lg text-text-2">{details.tagline}</p>}
        <TitleFactsRow details={details} />
      </DetailHeader>

      <DetailBody
        aside={
          <>
            <RequestCard
              cardRef={card}
              target={details}
              state={state}
              note={note}
              journeyTitle={journeyTitle(details.title, details.kind)}
              journeys={details.journeys}
              openIn={details.openIn}
              openInPrimary={openInPrimary}
              action={
                canRequestMovie ? (
                  <Button
                    variant="primary"
                    className="w-full"
                    aria-disabled={create.isPending || undefined}
                    onClick={(event) => {
                      if (create.isPending) return
                      submit(
                        { kind: details.kind, titleId: details.id },
                        event.currentTarget === document.activeElement ? card.current : null,
                      )
                    }}
                  >
                    {t('detail.requestMovie')}
                  </Button>
                ) : anyOpen && tab !== 'seasons' ? (
                  <Button
                    variant="secondary"
                    className="w-full"
                    onClick={() => {
                      setTab('seasons')
                      // After the panel shows: the first season that can be chosen.
                      window.requestAnimationFrame(() =>
                        document
                          .querySelector<HTMLElement>('[data-season-list] button[role="checkbox"]')
                          ?.focus(),
                      )
                    }}
                  >
                    {t('detail.chooseSeasons')}
                  </Button>
                ) : null
              }
              outcome={outcome && <Outcome created={outcome} />}
            >
              {isSeries && details.requestable && (
                <FollowSwitch titleRef={details} title={details.title} ended={ended} />
              )}
            </RequestCard>
            {details.trailerUrl && <TrailerLink url={details.trailerUrl} />}
          </>
        }
      >
        <DetailTabs tabs={tabs} value={tab} onValueChange={setTab} />
      </DetailBody>
    </article>
  )
}

/** Certification · runtime or seasons · release · rating with votes · genres. */
function TitleFactsRow({ details }: { details: TitleDetails }) {
  const { t } = useTranslation('member')
  const isSeries = details.kind === 'series'
  return (
    <Facts>
      {details.certification && <CertificationBadge rating={details.certification.rating} />}
      {isSeries && details.seasonCount != null && (
        <span className="type-data text-text">
          {t('discover.card.seasons', { count: details.seasonCount })}
        </span>
      )}
      {details.runtimeMinutes != null && (
        <span className="flex items-center gap-1.5">
          <ClockIcon aria-hidden className="size-3.5 text-text-3" strokeWidth={1.75} />
          <span className="type-data text-text">
            {isSeries
              ? t('detail.perEpisode', { runtime: formatMinutes(details.runtimeMinutes) })
              : formatMinutes(details.runtimeMinutes)}
          </span>
        </span>
      )}
      {(details.releaseDate || details.year != null) && (
        <span className="flex items-center gap-1.5">
          <CalendarIcon aria-hidden className="size-3.5 text-text-3" strokeWidth={1.75} />
          <span className="type-data text-text">
            {isSeries || !details.releaseDate ? details.year : formatDay(details.releaseDate)}
          </span>
        </span>
      )}
      {details.rating != null && details.rating > 0 && details.voteCount > 0 && (
        <span className="flex items-center gap-1.5">
          <StarIcon aria-hidden className="size-3.5 text-text-3" strokeWidth={1.75} />
          <span className="sr-only">{t('title.rating')}</span>
          <span className="type-data text-text">
            {formatNumber(details.rating, { minimumFractionDigits: 1, maximumFractionDigits: 1 })}
          </span>
          <span className="type-data text-xs text-text-3">
            {t('title.votes', { count: details.voteCount })}
          </span>
        </span>
      )}
      {details.genres.length > 0 && <span>{details.genres.join(', ')}</span>}
    </Facts>
  )
}

/** What the last request did: its state, what was left out, the way to "Requests". */
function Outcome({ created }: { created: CreatedRequest }) {
  const { t } = useTranslation('member')
  const { request, skipped } = created
  const unit = (season?: number | null) =>
    season == null ? t('title.movieUnit') : t('title.seasonLabel', { number: season })
  return (
    <div className="flex flex-col gap-1.5 rounded-card bg-night-2 px-4 py-3 text-sm text-text-2">
      <p className="font-medium text-text">
        {request.state === 'pending_approval'
          ? t('title.requestedPending')
          : t('title.requestedApproved')}
      </p>
      {skipped.length > 0 && (
        <p>
          <span className="text-text">{t('title.skipped')}</span>{' '}
          {skipped
            .map((s) => t(`title.skipReason.${s.reason}`, { unit: unit(s.season) }))
            .join('; ')}
        </p>
      )}
      <Link to="/requests" className="self-start text-text underline underline-offset-4">
        {t('title.seeRequests')}
      </Link>
    </div>
  )
}

function Overview({ details }: { details: TitleDetails }) {
  const { t } = useTranslation('member')
  const next = details.nextEpisode
  const groups = providerGroups(details.watchProviders)
  const watchId = useId()
  return (
    <div className="flex flex-col gap-8">
      <div className="flex flex-col gap-3">
        <p className="max-w-[64ch] text-lg leading-relaxed text-text">
          {details.overview || t('title.noOverview')}
        </p>
        {next && (
          <p className="text-base text-text-2">
            {t('detail.nextEpisodeLabel')}{' '}
            <span className="type-data text-text">
              {t('detail.episodeCode', { season: next.season, episode: next.episode })}
            </span>
            {next.name && <> · {next.name}</>}
            {next.airDate && (
              <>
                {' · '}
                <span className="type-data text-text">{formatUpcomingDay(next.airDate)}</span>
              </>
            )}
          </p>
        )}
      </div>
      <section aria-labelledby={watchId} className="flex flex-col gap-4">
        <SectionHeading id={watchId}>{t('detail.whereToWatch')}</SectionHeading>
        {groups.length === 0 ? (
          <p className="text-base text-text-3">{t('detail.noOffers')}</p>
        ) : (
          <div className="flex flex-col gap-4">
            {groups.map(([group, list]) => (
              <div key={group} className="flex flex-col gap-2 sm:flex-row sm:gap-4">
                <h4 className="w-16 shrink-0 pt-1 text-sm text-text-3">
                  {t(`detail.offers.${group}`)}
                </h4>
                <ul className="flex flex-wrap gap-3">
                  {list.map((p) => (
                    <li key={p.id} className="flex w-[4.5rem] flex-col items-center gap-1.5">
                      {p.logo ? (
                        <img
                          src={tmdbImage(p.logo, 'w92')}
                          alt=""
                          loading="lazy"
                          className="size-12 rounded-card border border-line bg-night-2 object-cover"
                        />
                      ) : (
                        <Monogram name={p.name} className="size-12 rounded-card text-sm" />
                      )}
                      <span className="line-clamp-2 text-center text-xs leading-tight text-text-2">
                        {p.name}
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            ))}
            <Credit attributions={details.attributions} data="watch_providers" />
          </div>
        )}
      </section>
    </div>
  )
}

function Cast({ details }: { details: TitleDetails }) {
  const { t } = useTranslation('member')
  const { leads } = crewGroups(details.crew)
  const directors = leads.filter((c) => c.role === 'director')
  const creators = leads.filter((c) => c.role === 'creator')
  return (
    <div className="flex flex-col gap-6">
      {(directors.length > 0 || creators.length > 0) && (
        <dl className="flex flex-col gap-1 text-base">
          {directors.length > 0 && (
            <div className="flex flex-wrap gap-x-2">
              <dt className="text-text-3">{t('detail.facts.directors')}</dt>
              <dd className="text-text">{directors.map((c) => c.name).join(', ')}</dd>
            </div>
          )}
          {creators.length > 0 && (
            <div className="flex flex-wrap gap-x-2">
              <dt className="text-text-3">{t('detail.facts.creators')}</dt>
              <dd className="text-text">{creators.map((c) => c.name).join(', ')}</dd>
            </div>
          )}
        </dl>
      )}
      {details.cast.length === 0 ? (
        <p className="text-base text-text-3">{t('detail.noCast')}</p>
      ) : (
        // Shown, not browsed: no person pages (0003 D12, spec 0008 Q15).
        <ul className="grid grid-cols-[repeat(auto-fill,minmax(6.5rem,1fr))] gap-x-4 gap-y-6">
          {details.cast.map((person, index) => (
            <li
              key={`${index}-${person.name}`}
              className="flex flex-col items-center gap-2 text-center"
            >
              {person.profile ? (
                <img
                  src={tmdbImage(person.profile, 'w185')}
                  alt=""
                  loading="lazy"
                  className="aspect-square w-20 rounded-full border border-line bg-night-2 object-cover sm:w-24"
                />
              ) : (
                <Monogram name={person.name} className="w-20 text-xl sm:w-24" />
              )}
              <span className="flex flex-col">
                <span className="text-sm font-semibold text-text">{person.name}</span>
                {person.character && (
                  <span className="text-xs text-text-3">{person.character}</span>
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

const STATUSES = ['Returning Series', 'Ended', 'Canceled', 'In Production', 'Planned', 'Pilot']

function Details({
  details,
  leads,
  writers,
}: {
  details: TitleDetails
  leads: TitleDetails['crew']
  writers: TitleDetails['crew']
}) {
  const { t } = useTranslation('member')
  const isSeries = details.kind === 'series'
  const status = details.seriesStatus
  const ids = details.details.externalIds
  const next = details.nextEpisode
  const names = (list: { name: string }[]) =>
    list.length > 0 ? list.map((c) => c.name).join(', ') : null
  return (
    <FactList
      rows={[
        [
          t('detail.facts.originalTitle'),
          details.originalTitle !== details.title && details.originalTitle,
        ],
        [
          t('detail.facts.originalLanguage'),
          details.details.originalLanguage && formatLanguage(details.details.originalLanguage),
        ],
        [
          isSeries ? t('detail.facts.firstAired') : t('detail.facts.released'),
          details.releaseDate && (
            <span key="v" className="type-data">
              {formatDay(details.releaseDate)}
            </span>
          ),
        ],
        [
          t('detail.facts.digital'),
          details.digitalReleaseDate && (
            <span key="v" className="type-data">
              {formatDay(details.digitalReleaseDate)}
            </span>
          ),
        ],
        [
          t('detail.facts.status'),
          status &&
            (STATUSES.includes(status)
              ? t(`detail.seriesStatus.${status.replace(/\s/g, '')}` as 'detail.seriesStatus.Ended')
              : status),
        ],
        [
          t('detail.facts.nextEpisode'),
          next && (
            <span key="v" className="type-data">
              {t('detail.episodeCode', { season: next.season, episode: next.episode })}
              {next.airDate && ` · ${formatUpcomingDay(next.airDate)}`}
            </span>
          ),
        ],
        [t('detail.facts.networks'), details.networks.join(', ')],
        [isSeries ? t('detail.facts.creators') : t('detail.facts.directors'), names(leads)],
        [t('detail.facts.writers'), names(writers)],
        [t('detail.facts.studios'), details.details.companies.join(', ')],
        [
          t('detail.facts.ids'),
          (ids.tmdb != null || ids.imdb || ids.tvdb != null) && (
            <span key="v" className="type-data flex flex-wrap gap-x-4 text-sm">
              {ids.tmdb != null && <span>TMDB {ids.tmdb}</span>}
              {ids.imdb && <span>IMDb {ids.imdb}</span>}
              {ids.tvdb != null && <span>TVDB {ids.tvdb}</span>}
            </span>
          ),
        ],
      ]}
    />
  )
}

/**
 * The Seasons tab: each season with its state and an "Episodes" disclosure; open ones can be
 * chosen. Gold only once something is chosen, "Everything is available" when nothing can be
 * (MEM-06). The list stays where it is after requesting (MEM-10).
 */
function Seasons({
  details,
  selected,
  onSelect,
  pending,
  onRequest,
}: {
  details: TitleDetails
  selected: Set<number>
  onSelect: (next: Set<number>) => void
  pending: boolean
  /** `focusAfter`: where focus goes once the button gives way (the selection bar). */
  onRequest: (focusAfter: HTMLElement | null) => void
}) {
  const { t } = useTranslation('member')
  const open = details.requestable ? selectableSeasons(details.seasons) : []
  const choosable = details.requestable
    ? details.seasons.filter((s) => seasonBlock(s) === null)
    : []
  const allChosen = open.length > 0 && open.every((n) => selected.has(n))
  const everythingThere = details.seasons.every((s) => s.status === 'available')
  const headingId = useId()
  if (details.seasons.length === 0) {
    return <p className="text-base text-text-3">{t('detail.seasons.none')}</p>
  }
  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <SectionHeading id={headingId}>{t('title.seasons')}</SectionHeading>
        {open.length > 1 && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => onSelect(toggleAll(details.seasons, selected))}
          >
            {allChosen ? t('title.selectNone') : t('title.selectAll')}
          </Button>
        )}
      </div>
      <ul data-season-list className="flex flex-col gap-2">
        {details.seasons.map((season) => (
          <SeasonRow
            key={season.number}
            details={details}
            season={season}
            checked={selected.has(season.number)}
            choosable={choosable.includes(season)}
            onChange={(on) => {
              const next = new Set(selected)
              if (on) next.add(season.number)
              else next.delete(season.number)
              onSelect(next)
            }}
          />
        ))}
      </ul>
      {details.requestable && (
        <SelectionBar
          action={
            selected.size > 0 && (
              <Button
                variant="primary"
                size="sm"
                aria-disabled={pending || undefined}
                onClick={(event) => {
                  if (pending) return
                  const hadFocus = event.currentTarget === document.activeElement
                  onRequest(
                    hadFocus
                      ? event.currentTarget.closest<HTMLElement>('[data-selection-bar]')
                      : null,
                  )
                }}
              >
                {t('title.requestSeasons', { count: selected.size })}
              </Button>
            )
          }
        >
          {selected.size > 0
            ? t('detail.seasons.selected', { count: selected.size })
            : choosable.length > 0
              ? t('detail.seasons.selectHint')
              : everythingThere
                ? t('detail.seasons.allAvailable')
                : t('detail.seasons.allTaken')}
        </SelectionBar>
      )}
    </section>
  )
}

function SeasonRow({
  details,
  season,
  checked,
  choosable,
  onChange,
}: {
  details: TitleDetails
  season: SeasonDetails
  checked: boolean
  choosable: boolean
  onChange: (on: boolean) => void
}) {
  const { t } = useTranslation('member')
  const [expanded, setExpanded] = useState(false)
  const id = `${useId()}season-${season.number}`
  const journey = details.journeys.find((j) => j.season === season.number)
  const block = seasonBlock(season)
  const year = season.airDate?.slice(0, 4)
  return (
    <li
      className={cn(
        'rounded-card border bg-night-1/60 transition-colors duration-(--dur-1)',
        // Neutral when chosen: rows of equal rank never get gold.
        checked ? 'border-text-2 bg-night-2' : 'border-line',
      )}
    >
      <div className="grid grid-cols-[1.5rem_minmax(0,1fr)] items-center gap-x-3 gap-y-2 px-3 py-2.5 sm:grid-cols-[1.5rem_minmax(0,1fr)_auto_auto]">
        <span className="grid size-6 place-items-center">
          {choosable ? (
            <Checkbox
              id={id}
              aria-labelledby={`${id}-name`}
              checked={checked}
              onCheckedChange={(v) => onChange(v === true)}
            />
          ) : block === 'available' ? (
            <CircleCheckIcon aria-hidden className="size-5 text-ok" strokeWidth={1.75} />
          ) : null}
        </span>
        <label
          htmlFor={choosable ? id : undefined}
          className={cn('flex min-w-0 flex-col', choosable && 'cursor-pointer')}
        >
          <span id={`${id}-name`} className="truncate text-base font-medium text-text">
            {season.name}
          </span>
          <span className="type-data text-xs text-text-3">
            {t('title.episodes', { count: season.episodeCount })}
            {' · '}
            {year ?? t('title.notAired')}
          </span>
        </label>
        <span className="col-start-2 justify-self-start sm:col-start-auto sm:justify-self-end">
          {journey ? (
            <JourneyStatusChip journey={journey.journey} />
          ) : season.status === 'requested' && season.stage ? (
            <RequestStateChip
              state="in_progress"
              stage={season.stage}
              arrivingIn={details.arrivingIn}
            />
          ) : block === 'mine' ? (
            <span className="text-sm text-text-3">{t('title.blockMine')}</span>
          ) : (
            season.status !== 'new' && <AvailabilityChip status={season.status} />
          )}
        </span>
        <Button
          size="sm"
          variant="ghost"
          aria-expanded={expanded}
          aria-controls={`${id}-episodes`}
          aria-label={t('detail.seasons.episodesOf', { season: season.name })}
          className="col-start-2 justify-self-start sm:col-start-auto"
          onClick={() => setExpanded((v) => !v)}
        >
          {t('detail.seasons.episodes')}
          <ChevronDownIcon
            aria-hidden
            className={cn(
              'motion-safe:transition-transform motion-safe:duration-(--dur-2)',
              expanded && 'rotate-180',
            )}
          />
        </Button>
      </div>
      <div id={`${id}-episodes`} hidden={!expanded}>
        {expanded && <Episodes seriesId={details.id} season={season.number} />}
      </div>
    </li>
  )
}

function Episodes({ seriesId, season }: { seriesId: string; season: number }) {
  const { t } = useTranslation('member')
  const errorMessage = useApiErrorMessage()
  const episodes = useSeasonEpisodes(seriesId, season, true)
  return (
    <div className="mx-3 mb-3 border-t border-line pt-2 sm:ml-12">
      {episodes.isPending ? (
        <div aria-busy className="flex flex-col gap-2 py-1">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-3/5" />
        </div>
      ) : episodes.isError && !isNotFound(episodes.error) ? (
        <QueryError
          message={errorMessage(episodes.error)}
          onRetry={() => void episodes.refetch()}
        />
      ) : !episodes.data || episodes.data.episodes.length === 0 ? (
        <p className="py-2 text-sm text-text-3">{t('detail.seasons.noEpisodes')}</p>
      ) : (
        <ol className="flex flex-col">
          {episodes.data.episodes.map((episode) => (
            <li
              key={episode.number}
              className="grid grid-cols-[2.75rem_minmax(0,1fr)_auto] items-baseline gap-x-3 py-1.5 text-sm"
            >
              <span className="type-data text-xs text-text-3">
                {t('detail.seasons.episodeNumber', { number: episode.number })}
              </span>
              <span className="flex min-w-0 flex-col">
                <span className="text-text">{episode.name}</span>
                {episode.airDate && (
                  <span className="type-data text-xs text-text-3">
                    {formatDay(episode.airDate)}
                  </span>
                )}
              </span>
              <span className="type-data text-xs text-text-2">
                {episode.runtimeMinutes != null && formatMinutes(episode.runtimeMinutes)}
              </span>
            </li>
          ))}
        </ol>
      )}
    </div>
  )
}
