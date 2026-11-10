import { LoaderCircleIcon, RefreshCwIcon, TriangleAlertIcon } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import {
  type CategoryName,
  type CategorySettings,
  type MediaKind,
  type MinimumAvailability,
  type PreviewItem,
  type Relevance,
  coverImage,
  isMusic,
  tmdbImage,
  useCatalogReference,
  useProviders,
  useRefreshSourceLists,
  useSaveFollowCap,
  useSaveRelevance,
  useSaveSourceSettings,
  useSourceLists,
  useTrendingPreview,
} from '@/api/categories'
import { useApiErrorMessage } from '@/api/errors'
import type { ReleaseType, SecondaryType } from '@/api/discover'
import { FormField } from '@/components/FormField'
import { UnsavedChangesGuard } from '@/components/UnsavedChanges'
import { MultiSelect, SelectField } from '@/components/MultiSelect'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { formatBytes } from '@/lib/format'
import { cn } from '@/lib/utils'

type CatalogRole = CategorySettings['catalogs'][number]
type SourceRole = NonNullable<CategorySettings['source']>

/** `value` once it stopped changing for `ms`. */
function useDebounced<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value)
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), ms)
    return () => clearTimeout(timer)
  }, [value, ms])
  return settled
}

const EMPTY: Relevance = {
  originalLanguages: [],
  providerRegion: null,
  providers: [],
  releaseRegion: null,
  excludedGenres: [],
}

function normalise(r: Relevance | null | undefined): Relevance {
  return { ...EMPTY, ...r }
}

/**
 * Relevance of a category's default catalog (D3): original languages,
 * release region, provider region with providers, hidden genres. Next to
 * it a live preview of the Trending lane with the values being edited.
 */
export function RelevanceEditor({
  category,
  kind,
  catalog,
  defaults,
}: {
  category: CategoryName
  kind: MediaKind
  catalog: CatalogRole
  defaults: Relevance
}) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const saved = catalog.relevance ? normalise(catalog.relevance) : null
  const [draft, setDraft] = useState<Relevance>(saved ?? normalise(defaults))
  const reference = useCatalogReference(catalog.serviceId)
  const region = draft.providerRegion ?? ''
  const providers = useProviders(catalog.serviceId, kind, region)
  const save = useSaveRelevance(category)
  const [failure, setFailure] = useState<string | null>(null)
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved)
  const set = (patch: Partial<Relevance>) => setDraft((d) => ({ ...d, ...patch }))

  const languages = useMemo(
    () =>
      (reference.data?.languages ?? []).map((l) => ({
        value: l.code,
        label: l.name,
        meta: l.code,
      })),
    [reference.data],
  )
  const genres = (reference.data?.genres ?? []).filter((g) => g.kind === kind)
  const providerOptions = (providers.data ?? []).map((p) => ({
    value: p.id,
    label: p.name,
    icon: p.logo ? (
      <img
        src={tmdbImage(p.logo, 'w45')}
        alt=""
        className="size-5 shrink-0 rounded-[4px] bg-night-0"
        loading="lazy"
      />
    ) : undefined,
  }))

  const onSave = async () => {
    setFailure(null)
    try {
      await save.mutateAsync({ roleId: catalog.id, relevance: draft })
      toast(t('categories.relevance.saved'))
    } catch (err) {
      setFailure(errorMessage(err))
    }
  }

  if (reference.isError) {
    return (
      <p role="alert" className="text-bad">
        {errorMessage(reference.error)}
      </p>
    )
  }

  return (
    <div className="grid gap-6 xl:grid-cols-[minmax(0,26rem)_minmax(0,1fr)]">
      {/* Leaving or switching category with unsaved changes asks first (ADM-11). */}
      <UnsavedChangesGuard when={dirty} />
      <form
        className="flex flex-col gap-4"
        aria-label={t('categories.relevance.title')}
        onSubmit={(e) => {
          e.preventDefault()
          void onSave()
        }}
      >
        <MultiSelect
          label={t('categories.relevance.languages')}
          placeholder={t('categories.relevance.anyLanguage')}
          options={languages}
          value={draft.originalLanguages ?? []}
          onChange={(originalLanguages) => set({ originalLanguages })}
          disabled={!reference.data}
          hint={t('categories.relevance.languagesHint')}
        />
        <SelectField
          label={t('categories.relevance.releaseRegion')}
          value={draft.releaseRegion ?? ''}
          onChange={(v) => set({ releaseRegion: v || null })}
          disabled={!reference.data}
          hint={t('categories.relevance.releaseRegionHint')}
        >
          <option value="">{t('categories.relevance.anyRegion')}</option>
          {reference.data?.countries.map((c) => (
            <option key={c.code} value={c.code}>
              {c.name}
            </option>
          ))}
        </SelectField>
        <SelectField
          label={t('categories.relevance.providerRegion')}
          value={region}
          onChange={(v) => set({ providerRegion: v || null, providers: [] })}
          disabled={!reference.data}
        >
          <option value="">{t('categories.relevance.noProviderRegion')}</option>
          {reference.data?.providerRegions.map((c) => (
            <option key={c.code} value={c.code}>
              {c.name}
            </option>
          ))}
        </SelectField>
        <MultiSelect
          label={t('categories.relevance.providers')}
          placeholder={
            region ? t('categories.relevance.anyProvider') : t('categories.relevance.pickRegion')
          }
          options={providerOptions}
          value={draft.providers ?? []}
          onChange={(ids) => set({ providers: ids })}
          disabled={!region || !providers.data}
          hint={<span className="text-xs">{t('categories.relevance.justwatch')}</span>}
        />
        <fieldset className="flex flex-col gap-1.5">
          <legend className="mb-1.5 text-sm font-medium text-text">
            {t('categories.relevance.hiddenGenres')}
          </legend>
          <div className="flex flex-wrap gap-1.5">
            {genres.map((g) => {
              const hidden = (draft.excludedGenres ?? []).includes(g.id)
              return (
                <button
                  key={g.id}
                  type="button"
                  aria-pressed={hidden}
                  onClick={() =>
                    set({
                      excludedGenres: hidden
                        ? (draft.excludedGenres ?? []).filter((id) => id !== g.id)
                        : [...(draft.excludedGenres ?? []), g.id],
                    })
                  }
                  className={cn(
                    'cursor-pointer rounded-full border px-3 py-1 text-sm transition-colors',
                    hidden
                      ? 'border-text-2 bg-night-2 text-text line-through decoration-text-3'
                      : 'border-line text-text-2 hover:border-text-3 hover:text-text',
                  )}
                >
                  {g.name}
                </button>
              )
            })}
          </div>
        </fieldset>
        {failure && (
          <p role="alert" className="text-sm text-bad">
            {failure}
          </p>
        )}
        <div className="flex flex-wrap items-center gap-3">
          <Button type="submit" variant="primary" disabled={save.isPending || !reference.data}>
            {save.isPending && (
              <LoaderCircleIcon aria-hidden className="motion-safe:animate-spin" />
            )}
            {saved ? t('categories.relevance.save') : t('categories.relevance.saveFirst')}
          </Button>
          {dirty && saved && (
            <Button type="button" variant="ghost" onClick={() => setDraft(saved)}>
              {t('categories.relevance.reset')}
            </Button>
          )}
          {!saved && (
            <span className="text-sm text-text-3">{t('categories.relevance.notSaved')}</span>
          )}
        </div>
      </form>
      <TrendingPreview serviceId={catalog.serviceId} kind={kind} relevance={draft} />
    </div>
  )
}

const RELEASE_TYPES: ReleaseType[] = ['album', 'ep', 'single', 'broadcast', 'other']
const SECONDARY_TYPES: SecondaryType[] = [
  'compilation',
  'soundtrack',
  'live',
  'remix',
  'dj_mix',
  'mixtape',
  'demo',
  'spokenword',
  'interview',
  'audiobook',
  'audio_drama',
  'field_recording',
]

function toggled<T>(list: T[] | undefined, value: T): T[] {
  const current = list ?? []
  return current.includes(value) ? current.filter((v) => v !== value) : [...current, value]
}

/**
 * Relevance of a music catalog (MusicBrainz, spec 0006 Q3): which release types count and
 * which kinds (live, compilations, ...) stay hidden. Starts at albums and EPs without any of
 * those kinds. Next to it the Trending preview, which these settings do not filter.
 */
export function MusicRelevanceEditor({
  category,
  catalog,
  defaults,
}: {
  category: CategoryName
  catalog: CatalogRole
  defaults: Relevance
}) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const pick = (r: Relevance): Relevance => ({
    releaseTypes: r.releaseTypes ?? [],
    excludedSecondaryTypes: r.excludedSecondaryTypes ?? [],
  })
  const saved = catalog.relevance ? pick(catalog.relevance) : null
  const [draft, setDraft] = useState<Relevance>(saved ?? pick(defaults))
  const save = useSaveRelevance(category)
  const [failure, setFailure] = useState<string | null>(null)
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved)

  const onSave = async () => {
    setFailure(null)
    try {
      await save.mutateAsync({ roleId: catalog.id, relevance: draft })
      toast(t('categories.relevance.saved'))
    } catch (err) {
      setFailure(errorMessage(err))
    }
  }

  const chip = (pressed: boolean, struck: boolean) =>
    cn(
      'cursor-pointer rounded-full border px-3 py-1 text-sm transition-colors',
      pressed
        ? cn('border-text-2 bg-night-2 text-text', struck && 'line-through decoration-text-3')
        : 'border-line text-text-2 hover:border-text-3 hover:text-text',
    )

  return (
    <div className="grid gap-6 xl:grid-cols-[minmax(0,26rem)_minmax(0,1fr)]">
      {/* Leaving or switching category with unsaved changes asks first (ADM-11). */}
      <UnsavedChangesGuard when={dirty} />
      <form
        className="flex flex-col gap-5"
        aria-label={t('categories.relevance.title')}
        onSubmit={(e) => {
          e.preventDefault()
          void onSave()
        }}
      >
        <p className="text-sm text-text-2">{t('categories.relevance.music.description')}</p>
        <fieldset className="flex flex-col gap-1.5">
          <legend className="mb-1.5 text-sm font-medium text-text">
            {t('categories.relevance.music.releaseTypes')}
          </legend>
          <div className="flex flex-wrap gap-1.5">
            {RELEASE_TYPES.map((type) => (
              <button
                key={type}
                type="button"
                aria-pressed={(draft.releaseTypes ?? []).includes(type)}
                onClick={() =>
                  setDraft((d) => ({ ...d, releaseTypes: toggled(d.releaseTypes, type) }))
                }
                className={chip((draft.releaseTypes ?? []).includes(type), false)}
              >
                {t(`categories.relevance.music.types.${type}`)}
              </button>
            ))}
          </div>
          <p className="text-sm text-text-3">{t('categories.relevance.music.releaseTypesHint')}</p>
        </fieldset>
        <fieldset className="flex flex-col gap-1.5">
          <legend className="mb-1.5 text-sm font-medium text-text">
            {t('categories.relevance.music.secondaryTypes')}
          </legend>
          <div className="flex flex-wrap gap-1.5">
            {SECONDARY_TYPES.map((type) => {
              const hidden = (draft.excludedSecondaryTypes ?? []).includes(type)
              return (
                <button
                  key={type}
                  type="button"
                  aria-pressed={hidden}
                  onClick={() =>
                    setDraft((d) => ({
                      ...d,
                      excludedSecondaryTypes: toggled(d.excludedSecondaryTypes, type),
                    }))
                  }
                  className={chip(hidden, true)}
                >
                  {t(`categories.relevance.music.secondary.${type}`)}
                </button>
              )
            })}
          </div>
          <p className="text-sm text-text-3">
            {t('categories.relevance.music.secondaryTypesHint')}
          </p>
        </fieldset>
        {failure && (
          <p role="alert" className="text-sm text-bad">
            {failure}
          </p>
        )}
        <div className="flex flex-wrap items-center gap-3">
          <Button type="submit" variant="primary" disabled={save.isPending}>
            {save.isPending && (
              <LoaderCircleIcon aria-hidden className="motion-safe:animate-spin" />
            )}
            {saved ? t('categories.relevance.save') : t('categories.relevance.saveFirst')}
          </Button>
          {dirty && saved && (
            <Button type="button" variant="ghost" onClick={() => setDraft(saved)}>
              {t('categories.relevance.reset')}
            </Button>
          )}
          {!saved && (
            <span className="text-sm text-text-3">{t('categories.relevance.notSaved')}</span>
          )}
        </div>
      </form>
      <TrendingPreview serviceId={catalog.serviceId} kind="album" relevance={draft} />
    </div>
  )
}

/** First page of the Trending lane for `relevance`, refreshed as it changes. */
export function TrendingPreview({
  serviceId,
  kind,
  relevance,
}: {
  serviceId: number
  kind: MediaKind
  relevance: Relevance
}) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const settled = useDebounced(relevance, 400)
  const preview = useTrendingPreview(serviceId, kind, settled)
  const items = preview.data ?? []
  return (
    <section aria-labelledby="trending-preview" className="flex min-w-0 flex-col gap-3">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 id="trending-preview" className="type-display text-lg font-bold text-text">
          {t('categories.preview.title')}
        </h3>
        <span className="text-sm text-text-3">
          {preview.isFetching ? (
            <span className="inline-flex items-center gap-1.5">
              <LoaderCircleIcon aria-hidden className="size-3.5 motion-safe:animate-spin" />
              {t('categories.preview.updating')}
            </span>
          ) : (
            t('categories.preview.count', { count: items.length })
          )}
        </span>
      </div>
      <p className="text-sm text-text-3">
        {isMusic(kind) ? t('categories.preview.hintMusic') : t('categories.preview.hint')}
      </p>
      {preview.isError && (
        <p role="alert" className="text-sm text-bad">
          {errorMessage(preview.error)}
        </p>
      )}
      {preview.isSuccess && items.length === 0 && (
        <p className="rounded-card border border-dashed border-line px-4 py-8 text-center text-text-3">
          {t('categories.preview.empty')}
        </p>
      )}
      <ul
        aria-label={t('categories.preview.title')}
        className={cn(
          'grid grid-cols-3 gap-3 sm:grid-cols-4 lg:grid-cols-5 2xl:grid-cols-6',
          preview.isFetching && 'opacity-70 transition-opacity',
        )}
      >
        {items.map((item) => (
          <PreviewPoster key={item.id} item={item} />
        ))}
      </ul>
    </section>
  )
}

function PreviewPoster({ item }: { item: PreviewItem }) {
  const music = isMusic(item.kind)
  const [failed, setFailed] = useState(false)
  return (
    <li className="flex min-w-0 flex-col gap-1.5">
      <div
        className={cn(
          'overflow-hidden rounded-card border border-line bg-night-2',
          music ? 'aspect-square' : 'aspect-[2/3]',
        )}
      >
        {item.poster && !failed ? (
          <img
            onError={() => setFailed(true)}
            src={music ? coverImage(item.poster, '250') : tmdbImage(item.poster, 'w342')}
            alt=""
            loading="lazy"
            className="size-full object-cover"
          />
        ) : (
          <div className="flex size-full items-center justify-center p-2 text-center text-xs text-text-3">
            {item.title}
          </div>
        )}
      </div>
      <div className="min-w-0">
        <p className="truncate text-sm font-medium text-text" title={item.title}>
          {item.title}
        </p>
        {/* Charts carry no year or language for albums. */}
        {!(music && item.year == null) && (
          <p className="type-data text-xs text-text-3">
            {music ? item.year : `${item.year ?? '–'} · ${item.originalLanguage}`}
          </p>
        )}
      </div>
    </li>
  )
}

const AVAILABILITY: MinimumAvailability[] = ['announced', 'inCinemas', 'released']

/**
 * Quality profile, root folder, (movies) minimum availability and (music,
 * Lidarr) metadata profile of the category's source (D9, spec 0006 Q6),
 * picked from the lists the worker reads from the source. Opening the form
 * asks for a fresh read. The only option of a list is preselected while
 * nothing is chosen; until everything is saved (`requestsReady`, the
 * server's rule) requests stay closed. A metadata profile that drops albums
 * or EPs is flagged: Lidarr could remove a requested album on its next
 * refresh.
 */
export function SourceSettingsForm({
  category,
  source,
  requestFlow = true,
  requestsReady,
}: {
  category: CategoryName
  source: SourceRole
  /** The category has requests at all (adult not before spec 0007 slice 4). */
  requestFlow?: boolean
  requestsReady: boolean
}) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const refresh = useRefreshSourceLists()
  // While set: the `attemptedAt` before the read we asked for.
  const [waitingSince, setWaitingSince] = useState<string | null | undefined>(undefined)
  const lists = useSourceLists(source.serviceId, waitingSince !== undefined)
  const save = useSaveSourceSettings(category)
  const [profile, setProfile] = useState(source.settings.qualityProfileId?.toString() ?? '')
  const [folder, setFolder] = useState(source.settings.rootFolderPath ?? '')
  const [availability, setAvailability] = useState<MinimumAvailability>(
    source.settings.minimumAvailability ?? 'released',
  )
  const [metadata, setMetadata] = useState(source.settings.metadataProfileId?.toString() ?? '')
  const [failure, setFailure] = useState<string | null>(null)
  const isMovies = category === 'movies'
  const requested = useRef(false)
  // The read asked for on opening has come back (or timed out).
  const [fresh, setFresh] = useState(false)

  const reread = async () => {
    const before = lists.data?.attemptedAt ?? null
    try {
      await refresh.mutateAsync(source.serviceId)
      setWaitingSince(before)
    } catch (err) {
      setFailure(errorMessage(err))
    }
  }
  // Read the lists fresh once per opening (lesson 32: read, not remembered).
  useEffect(() => {
    if (requested.current || !lists.isFetched) return
    requested.current = true
    void reread()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lists.isFetched])
  useEffect(() => {
    if (waitingSince === undefined || !lists.data) return
    if ((lists.data.attemptedAt ?? null) !== waitingSince) {
      setWaitingSince(undefined)
      setFresh(true)
    }
  }, [lists.data, waitingSince])
  useEffect(() => {
    if (waitingSince === undefined) return
    const timer = setTimeout(() => {
      setWaitingSince(undefined)
      setFresh(true)
    }, 20_000)
    return () => clearTimeout(timer)
  }, [waitingSince])

  const reading = waitingSince !== undefined || refresh.isPending
  const data = lists.data
  // One option only: nothing to choose, so it is filled in (still saved by
  // the admin). Once per opening, from the fresh read; a choice already
  // made (or cleared afterwards) is never replaced.
  const preselected = useRef(false)
  useEffect(() => {
    if (!data || !fresh || preselected.current) return
    preselected.current = true
    const [onlyProfile, ...otherProfiles] = data.qualityProfiles
    if (onlyProfile && otherProfiles.length === 0) {
      setProfile((current) => current || onlyProfile.id.toString())
    }
    const [onlyFolder, ...otherFolders] = data.rootFolders.filter((f) => f.accessible)
    if (onlyFolder && otherFolders.length === 0) {
      setFolder((current) => current || onlyFolder.path)
    }
    const [onlyMetadata, ...otherMetadata] = data.metadataProfiles
    if (onlyMetadata && otherMetadata.length === 0) {
      setMetadata((current) => current || onlyMetadata.id.toString())
    }
  }, [data, fresh])
  // Only Lidarr has metadata profiles; the picker shows only for it.
  const hasMetadata = (data?.metadataProfiles.length ?? 0) > 0 || metadata !== ''
  const chosenMetadata = data?.metadataProfiles.find((p) => p.id.toString() === metadata)

  const onSave = async () => {
    setFailure(null)
    try {
      await save.mutateAsync({
        qualityProfileId: profile ? Number(profile) : null,
        rootFolderPath: folder || null,
        minimumAvailability: isMovies ? availability : null,
        // Lidarr only: Radarr and Sonarr refuse a metadata profile.
        ...(hasMetadata ? { metadataProfileId: metadata ? Number(metadata) : null } : {}),
      })
      toast(t('categories.source.saved'))
    } catch (err) {
      setFailure(errorMessage(err))
    }
  }

  return (
    <form
      aria-label={t('categories.source.settingsTitle')}
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault()
        void onSave()
      }}
    >
      <div className="flex flex-wrap items-center gap-2 text-sm text-text-3">
        {reading ? (
          <StatusChip tone="info">{t('categories.source.reading')}</StatusChip>
        ) : data?.error ? (
          <StatusChip tone="bad">{t('categories.source.readFailed')}</StatusChip>
        ) : data?.fetchedAt ? (
          <StatusChip tone="ok">{t('categories.source.readOk')}</StatusChip>
        ) : null}
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => void reread()}
          disabled={reading}
        >
          <RefreshCwIcon aria-hidden className={cn(reading && 'motion-safe:animate-spin')} />
          {t('categories.source.reread')}
        </Button>
      </div>
      {/* Every category has its request flow (spec 0007 slice 4): no roadmap text (ADM-18). */}
      {requestFlow && !requestsReady && (
        <p role="note" className="flex items-start gap-2 text-sm text-warn">
          <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
          {t(
            hasMetadata
              ? 'categories.source.requestsClosedMetadata'
              : 'categories.source.requestsClosed',
          )}
        </p>
      )}
      <div className="grid gap-4 sm:grid-cols-2">
        <SelectField
          label={t('categories.source.qualityProfile')}
          value={profile}
          onChange={setProfile}
          disabled={!data?.qualityProfiles.length}
        >
          <option value="">{t('categories.source.choose')}</option>
          {data?.qualityProfiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </SelectField>
        <SelectField
          label={t('categories.source.rootFolder')}
          value={folder}
          onChange={setFolder}
          disabled={!data?.rootFolders.length}
        >
          <option value="">{t('categories.source.choose')}</option>
          {data?.rootFolders.map((f) => (
            <option key={f.path} value={f.path} disabled={!f.accessible}>
              {f.path}
              {f.freeSpaceBytes != null
                ? ` · ${t('categories.source.free', { size: formatBytes(f.freeSpaceBytes) })}`
                : ''}
              {!f.accessible ? ` · ${t('categories.source.unreachable')}` : ''}
            </option>
          ))}
        </SelectField>
        {hasMetadata && (
          <SelectField
            label={t('categories.source.metadataProfile')}
            value={metadata}
            onChange={setMetadata}
            disabled={!data?.metadataProfiles.length}
            hint={
              chosenMetadata && !chosenMetadata.admitsAlbumsAndEps ? (
                <span role="alert" className="flex items-start gap-1.5 text-warn">
                  <TriangleAlertIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
                  {t('categories.source.metadataProfileExcludes')}
                </span>
              ) : (
                t('categories.source.metadataProfileHint')
              )
            }
          >
            <option value="">{t('categories.source.choose')}</option>
            {data?.metadataProfiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
                {p.admitsAlbumsAndEps ? '' : ` · ${t('categories.source.metadataProfileNarrow')}`}
              </option>
            ))}
          </SelectField>
        )}
        {isMovies && (
          <SelectField
            label={t('categories.source.minimumAvailability')}
            value={availability}
            onChange={(v) => setAvailability(v as MinimumAvailability)}
            hint={t('categories.source.minimumAvailabilityHint')}
          >
            {AVAILABILITY.map((a) => (
              <option key={a} value={a}>
                {t(`categories.source.availability.${a}`)}
              </option>
            ))}
          </SelectField>
        )}
      </div>
      {failure && (
        <p role="alert" className="text-sm text-bad">
          {failure}
        </p>
      )}
      <div>
        <Button type="submit" variant="secondary" disabled={save.isPending}>
          {t('categories.source.save')}
        </Button>
      </div>
    </form>
  )
}

/**
 * The private category's weekly cap: how many scenes one performer or studio follow requests
 * on its own per week (spec 0007 Q4, admin story 12; default 10, 1 to 100). Applies from the
 * next daily check on.
 */
export function FollowCapForm({
  category,
  catalog,
}: {
  category: CategoryName
  catalog: CatalogRole & { followCapPerWeek: number }
}) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const save = useSaveFollowCap(category)
  const [draft, setDraft] = useState(String(catalog.followCapPerWeek))
  const [failure, setFailure] = useState<string | null>(null)
  const value = Number(draft)
  const valid = Number.isInteger(value) && value >= 1 && value <= 100
  const dirty = valid && value !== catalog.followCapPerWeek

  const onSave = async () => {
    setFailure(null)
    try {
      await save.mutateAsync({ roleId: catalog.id, perWeek: value })
      toast(t('categories.followCap.saved'))
    } catch (err) {
      setFailure(errorMessage(err))
    }
  }

  return (
    <form
      className="flex max-w-md flex-col gap-4"
      aria-label={t('categories.followCap.title')}
      onSubmit={(e) => {
        e.preventDefault()
        if (dirty) void onSave()
      }}
    >
      <UnsavedChangesGuard when={dirty} />
      <FormField
        label={t('categories.followCap.label')}
        hint={t('categories.followCap.hint')}
        type="number"
        inputMode="numeric"
        min={1}
        max={100}
        step={1}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        className="type-data w-28"
      />
      {!valid && <p className="text-sm text-bad">{t('categories.followCap.range')}</p>}
      {failure && <p className="text-sm text-bad">{failure}</p>}
      <div>
        <Button type="submit" size="sm" disabled={!dirty || save.isPending}>
          {t('categories.followCap.save')}
        </Button>
      </div>
    </form>
  )
}
