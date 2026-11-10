import { CalendarIcon, DiscIcon } from 'lucide-react'
import { type CSSProperties, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { coverImage } from '@/api/categories'
import {
  type AlbumDetails,
  type ArtistDetails,
  type Availability,
  type DiscographyAlbum,
  useAlbum,
  useArtist,
  useCreateRequest,
} from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { DetailLoad, MorphTarget } from '@/components/detail/states'
import { useDetailDocumentTitle, useDetailMemory } from '@/lib/detailStage'
import {
  CategoryLine,
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
} from '@/components/detail/parts'
import { Tracklist } from '@/components/Tracklist'
import { CoverFallback } from '@/components/CoverFallback'
import { Cover } from '@/components/Poster'
import { AvailabilityChip } from '@/components/RequestStatus'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { discs } from '@/lib/detail'
import { formatDay, formatNumber, formatSeconds } from '@/lib/format'
import { useJourneyTitle } from '@/lib/journey'
import { focusElement } from '@/lib/returnFocus'
import { coverLift } from '@/lib/motion'
import { discographyGroups } from '@/lib/music'
import { cardState, unitsState } from '@/lib/states'
import { sampleCoverTint } from '@/lib/tint'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { cn } from '@/lib/utils'

/** Whether a member can still ask for an album: not there and not asked for by them. */
function canRequest(status: Availability, mine: boolean) {
  return status !== 'available' && !mine
}

/** "Genres from MusicBrainz (CC BY-NC-SA)", linking to the licence (spec 0008 Q17 B). */
function GenresCredit({ attributions }: { attributions: AlbumDetails['attributions'] }) {
  const { t } = useTranslation('member')
  return (
    <Credit attributions={attributions} data="genres">
      {(credit) =>
        t('detail.genresCredit', { source: credit.source, licence: credit.licence ?? '' })
      }
    </Credit>
  )
}

/** An album in the detail overlay: flat cover tint, tracklist first (A7, prototype 02-B + C). */
export function AlbumDetail({ id }: { id: string }) {
  const album = useAlbum(id)
  if (!album.data) {
    return <DetailLoad kind="album" error={album.error} onRetry={() => void album.refetch()} />
  }
  return <AlbumView album={album.data} />
}

function AlbumView({ album }: { album: AlbumDetails }) {
  const { t } = useTranslation(['member', 'common'])
  const journeyTitle = useJourneyTitle()
  const { open } = useTitleSheet()
  const create = useCreateRequest()
  const errorMessage = useApiErrorMessage()
  const card = useRef<HTMLElement>(null)
  const [tab, setTab] = useState('tracks')
  // The tint lives in this view only: sampled from the loaded cover, never stored (A1).
  const [tint, setTint] = useState<string | null>(null)
  const [failed, setFailed] = useState(false)
  useDetailDocumentTitle(album.title)

  const { totalSeconds } = discs(album.tracks)
  const requestable = album.requestable && canRequest(album.status, album.requestedByMe)
  const state = cardState(album)
  const note = !album.requestable
    ? t('music.notRequestable')
    : album.status === 'available' && album.openIn.length === 0
      ? t('music.isAvailable')
      : album.status === 'requested' && !album.requestedByMe && album.journeys.length === 0
        ? t('title.isRequestedByOthers')
        : requestable
          ? t('detail.requestHint')
          : null

  const artistLink = (className?: string) =>
    album.artistId ? (
      <button
        type="button"
        onClick={() => album.artistId && open({ kind: 'artist', id: album.artistId })}
        className={cn(
          'cursor-pointer rounded-control text-left text-text underline decoration-text-3 underline-offset-4 hover:decoration-text',
          className,
        )}
      >
        {album.artist}
      </button>
    ) : (
      <span className={className}>{album.artist}</span>
    )

  const surface: CSSProperties | undefined = tint
    ? ({ '--tint': tint } as CSSProperties)
    : undefined

  return (
    <article
      style={surface}
      className={cn('min-h-full', tint && 'detail-tint')}
      data-tinted={tint ? '' : undefined}
    >
      <DetailHeader
        backdrop={null}
        square
        cover={
          <MorphTarget>
            {album.cover && !failed ? (
              <img
                src={coverImage(album.cover, '500')}
                srcSet={`${coverImage(album.cover, '250')} 250w, ${coverImage(album.cover, '500')} 500w`}
                sizes="(min-width: 640px) 176px, 112px"
                alt=""
                decoding="async"
                onLoad={(event) => setTint(sampleCoverTint(event.currentTarget))}
                onError={() => setFailed(true)}
                className="aspect-square w-full rounded-card bg-night-2 object-cover shadow-[0_12px_28px_rgb(0_0_0/0.45)] ring-1 ring-text/10"
              />
            ) : (
              <CoverFallback
                title={album.title}
                category="music"
                className="aspect-square rounded-card"
              />
            )}
          </MorphTarget>
        }
      >
        <CategoryLine category="music" />
        <DetailTitle>{album.title}</DetailTitle>
        {album.artist && artistLink('self-start text-lg')}
        <Facts>
          {album.releaseDate ? (
            <span className="flex items-center gap-1.5">
              <CalendarIcon aria-hidden className="size-3.5 text-text-3" strokeWidth={1.75} />
              <span className="type-data text-text">{formatDay(album.releaseDate)}</span>
            </span>
          ) : (
            album.year != null && <span className="type-data text-text">{album.year}</span>
          )}
          {album.trackCount != null && (
            <span className="flex items-center gap-1.5">
              <DiscIcon aria-hidden className="size-3.5 text-text-3" strokeWidth={1.75} />
              <span className="type-data text-text">
                {t('music.tracks', { count: album.trackCount })}
                {totalSeconds != null && ` · ${formatSeconds(totalSeconds)}`}
              </span>
            </span>
          )}
          {album.label && <span>{album.label}</span>}
          {album.releaseType && <span>{t(`music.types.${album.releaseType}`)}</span>}
          {album.secondaryTypes.map((s) => (
            <StatusChip key={s} tone="neutral">
              {t(`music.secondary.${s}`)}
            </StatusChip>
          ))}
        </Facts>
      </DetailHeader>

      <DetailBody
        aside={
          <RequestCard
            cardRef={card}
            target={{ kind: 'album', id: album.id }}
            state={state}
            note={note}
            journeyTitle={journeyTitle(album.artist, 'artist', null, album.title)}
            journeys={album.journeys}
            openIn={album.openIn}
            openInPrimary={!requestable}
            action={
              requestable && (
                <Button
                  variant="primary"
                  className="w-full"
                  aria-disabled={create.isPending || undefined}
                  onClick={(event) => {
                    if (create.isPending) return
                    const hadFocus = event.currentTarget === document.activeElement
                    create.mutate(
                      { kind: 'album', titleId: album.id },
                      {
                        onSuccess: ({ request }) => {
                          const pending = request.state === 'pending_approval'
                          toast(
                            t(
                              pending
                                ? 'discover.card.requestedPending'
                                : 'discover.card.requestedApproved',
                              { title: album.title },
                            ),
                          )
                          if (hadFocus && card.current) focusElement(card.current, { quiet: true })
                        },
                        onError: (error) => toast.error(errorMessage(error)),
                      },
                    )
                  }}
                >
                  {t('detail.requestAlbum')}
                </Button>
              )
            }
          />
        }
      >
        <DetailTabs
          value={tab}
          onValueChange={setTab}
          tabs={[
            {
              value: 'tracks',
              label: t('detail.tabs.tracks'),
              content: <Tracklist album={album} />,
            },
            {
              value: 'details',
              label: t('detail.tabs.details'),
              content: (
                <div className="flex flex-col gap-6">
                  {album.disambiguation && (
                    <p className="max-w-prose text-base text-text-2">{album.disambiguation}</p>
                  )}
                  <FactList
                    rows={[
                      [t('detail.facts.artist'), album.artist && artistLink()],
                      [
                        t('detail.facts.released'),
                        album.releaseDate && (
                          <span key="v" className="type-data">
                            {formatDay(album.releaseDate)}
                          </span>
                        ),
                      ],
                      [t('detail.facts.label'), album.label],
                      [
                        t('detail.facts.type'),
                        album.releaseType && t(`music.types.${album.releaseType}`),
                      ],
                      [
                        t('detail.facts.length'),
                        totalSeconds != null && (
                          <span key="v" className="type-data">
                            {formatSeconds(totalSeconds)}
                          </span>
                        ),
                      ],
                      [t('detail.facts.genres'), album.genres.join(', ')],
                    ]}
                  />
                  {album.genres.length > 0 && <GenresCredit attributions={album.attributions} />}
                </div>
              ),
            },
          ]}
        />
      </DetailBody>
    </article>
  )
}

/** An artist: round monogram, discography with state, album requests, follow switch (A7). */
export function ArtistDetail({ id }: { id: string }) {
  const artist = useArtist(id)
  if (!artist.data) {
    return <DetailLoad kind="artist" error={artist.error} onRetry={() => void artist.refetch()} />
  }
  return <ArtistView artist={artist.data} />
}

function ArtistView({ artist }: { artist: ArtistDetails }) {
  const { t } = useTranslation(['member', 'common'])
  const journeyTitle = useJourneyTitle()
  const create = useCreateRequest()
  const errorMessage = useApiErrorMessage()
  const card = useRef<HTMLElement>(null)
  const [tab, setTab] = useState('discography')
  const discographyId = useId()
  // Ticks and "Show all" survive a visit to an album and back (MEM-29).
  const [all, setAll] = useDetailMemory(`artist:${artist.id}:all`, false)
  const [picked, setPicked] = useDetailMemory<string[]>(`artist:${artist.id}:picked`, [])
  useDetailDocumentTitle(artist.name)
  const titleRef = { kind: 'artist', id: artist.id } as const
  const choosable = (a: DiscographyAlbum) =>
    artist.requestable && canRequest(a.status, a.requestedByMe)
  // Only what can still be chosen counts: a request may have taken some meanwhile.
  const selected = new Set(
    picked.filter((pid) => artist.albums.some((a) => a.id === pid && choosable(a))),
  )
  const hidden = artist.albums.filter((a) => !a.inDefaultView).length
  const groups = discographyGroups(artist.albums, all)
  const anyChoosable = artist.albums.some(choosable)
  const state = unitsState(artist.journeys)

  // Showing fewer drops the hidden albums from the choice: nothing goes out unseen.
  const collapse = () => {
    if (all) {
      const shown = new Set(artist.albums.filter((a) => a.inDefaultView).map((a) => a.id))
      setPicked([...selected].filter((pid) => shown.has(pid)))
    }
    setAll(!all)
  }
  const toggle = (albumId: string, on: boolean) => {
    const next = new Set(selected)
    if (on) next.add(albumId)
    else next.delete(albumId)
    setPicked([...next])
  }
  const submit = (focusAfter: HTMLElement | null) =>
    create.mutate(
      {
        kind: 'artist',
        titleId: artist.id,
        // Discography order, newest first.
        albums: artist.albums.filter((a) => selected.has(a.id)).map((a) => a.id),
      },
      {
        onSuccess: ({ request }) => {
          setPicked([])
          const pending = request.state === 'pending_approval'
          toast(
            t(pending ? 'discover.card.requestedPending' : 'discover.card.requestedApproved', {
              title: artist.name,
            }),
          )
          if (focusAfter?.isConnected) focusElement(focusAfter, { quiet: true })
        },
        onError: (error) => toast.error(errorMessage(error)),
      },
    )

  return (
    <article>
      <DetailHeader
        backdrop={null}
        square
        cover={
          <MorphTarget>
            <Monogram name={artist.name} className="w-full text-4xl sm:text-5xl" />
          </MorphTarget>
        }
      >
        <CategoryLine category="music" extra={t('journey.artist', { ns: 'common' })} />
        <DetailTitle>{artist.name}</DetailTitle>
        {artist.disambiguation && <p className="text-base text-text-2">{artist.disambiguation}</p>}
        <Facts>
          <span className="type-data text-text">
            {t('detail.releases', { count: artist.albums.length })}
          </span>
        </Facts>
      </DetailHeader>

      <DetailBody
        aside={
          <RequestCard
            cardRef={card}
            target={titleRef}
            state={state}
            note={
              !artist.requestable
                ? t('music.notRequestable')
                : anyChoosable && selected.size === 0
                  ? t('detail.chooseAlbumsHint')
                  : null
            }
            journeyTitle={journeyTitle(artist.name, 'artist')}
            journeys={artist.journeys}
            albums={Object.fromEntries(artist.albums.map((a) => [a.id, a.title]))}
          >
            {artist.requestable && <FollowSwitch titleRef={titleRef} title={artist.name} />}
          </RequestCard>
        }
      >
        <DetailTabs
          value={tab}
          onValueChange={setTab}
          tabs={[
            {
              value: 'discography',
              label: t('detail.tabs.discography'),
              content: (
                <section aria-labelledby={discographyId} className="flex flex-col gap-6">
                  <div className="flex flex-wrap items-baseline justify-between gap-2">
                    <SectionHeading id={discographyId}>{t('music.discography')}</SectionHeading>
                    {hidden > 0 && (
                      <Button variant="ghost" size="sm" aria-pressed={all} onClick={collapse}>
                        {all ? t('music.showFewer') : t('music.showAll', { count: hidden })}
                      </Button>
                    )}
                  </div>
                  {groups.length === 0 && (
                    <p className="rounded-card border border-dashed border-line px-4 py-8 text-center text-text-3">
                      {artist.albums.length === 0
                        ? t('music.noReleases')
                        : t('music.noDefaultReleases')}
                    </p>
                  )}
                  {groups.map(([group, items]) => (
                    <div key={group} className="flex flex-col gap-3">
                      <h4 className="text-sm font-medium text-text-2">
                        {t(`music.groups.${group}`)}
                      </h4>
                      <ul className="grid grid-cols-2 gap-x-3 gap-y-5 sm:grid-cols-4">
                        {items.map((album) => (
                          <li key={album.id}>
                            <AlbumTile
                              album={album}
                              selected={choosable(album) ? selected.has(album.id) : null}
                              onSelect={(on) => toggle(album.id, on)}
                            />
                          </li>
                        ))}
                      </ul>
                    </div>
                  ))}
                  {artist.requestable && (
                    <SelectionBar
                      action={
                        selected.size > 0 && (
                          <Button
                            variant="primary"
                            size="sm"
                            aria-disabled={create.isPending || undefined}
                            onClick={(event) => {
                              if (create.isPending) return
                              submit(
                                event.currentTarget === document.activeElement
                                  ? event.currentTarget.closest<HTMLElement>('[data-selection-bar]')
                                  : null,
                              )
                            }}
                          >
                            {t('music.requestAlbums', { count: selected.size })}
                          </Button>
                        )
                      }
                    >
                      {selected.size > 0
                        ? t('detail.albums.selected', { count: selected.size })
                        : anyChoosable
                          ? t('detail.albums.selectHint')
                          : artist.albums.every((a) => a.status === 'available')
                            ? t('detail.albums.allAvailable')
                            : t('detail.albums.allTaken')}
                    </SelectionBar>
                  )}
                </section>
              ),
            },
            {
              value: 'details',
              label: t('detail.tabs.details'),
              content: (
                <div className="flex flex-col gap-6">
                  <FactList
                    rows={[
                      [t('detail.facts.disambiguation'), artist.disambiguation],
                      [t('detail.facts.genres'), artist.genres.join(', ')],
                      [
                        t('detail.facts.releases'),
                        <span key="n" className="type-data">
                          {formatNumber(artist.albums.length)}
                        </span>,
                      ],
                    ]}
                  />
                  {artist.genres.length > 0 && <GenresCredit attributions={artist.attributions} />}
                </div>
              ),
            },
          ]}
        />
      </DetailBody>
    </article>
  )
}

/**
 * An album of the discography: opens the album; with `selected` set (it can be requested) a
 * checkbox picks it; the chip shows its state (the journey is in the request card).
 */
function AlbumTile({
  album,
  selected,
  onSelect,
}: {
  album: DiscographyAlbum
  selected: boolean | null
  onSelect: (on: boolean) => void
}) {
  const { t } = useTranslation('member')
  const { open } = useTitleSheet()
  return (
    // The checkbox sits outside the tile's `group`: its focus must not light up the cover.
    <div className="relative flex flex-col gap-1.5">
      <div className="group flex flex-col gap-1.5">
        <button
          type="button"
          onClick={(event) => {
            event.currentTarget.focus({ preventScroll: true })
            open({ kind: 'album', id: album.id })
          }}
          className="peer flex w-full cursor-pointer flex-col gap-1.5 rounded-card text-left focus-visible:outline-none"
        >
          <span
            className={cn(
              'block rounded-card ring-polaris ring-offset-2 ring-offset-night-1 group-has-[.peer:focus-visible]:ring-2',
              coverLift('peer'),
            )}
          >
            <Cover kind="album" file={album.cover} title={album.title} sizes="160px" />
          </span>
          <span className="line-clamp-2 text-sm leading-snug font-medium text-text">
            {album.title}
          </span>
          {album.year != null && (
            <span className="type-data text-xs text-text-3">{album.year}</span>
          )}
        </button>
        {album.status !== 'new' && (
          <span className="self-start">
            <AvailabilityChip
              status={album.status}
              requestedByMe={album.requestedByMe}
              className="text-xs"
            />
          </span>
        )}
      </div>
      {selected !== null && (
        // Outside the tile's button (no control in a button), over the cover's corner.
        // The whole 32 px circle is the target (WCAG 2.5.8), not only the 16 px box.
        <label className="absolute top-1.5 left-1.5 grid size-8 cursor-pointer place-items-center rounded-full bg-night-0/90">
          <Checkbox
            aria-label={t('music.select', { title: album.title })}
            checked={selected}
            onCheckedChange={(on) => onSelect(on === true)}
          />
        </label>
      )}
    </div>
  )
}
