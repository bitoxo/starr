import { ArrowLeftRightIcon, PlusIcon, StarIcon, Trash2Icon, TriangleAlertIcon } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

import {
  type CategoryName,
  type CategorySettings,
  type RoleKind,
  type SourceSwitchPreview,
  fetchSourceSwitchPreview,
  mediaKindOf,
  useAddRole,
  useCategories,
  useRemoveRole,
  useSetCategoryEnabled,
  useSetDefaultCatalog,
  useSetSource,
} from '@/api/categories'
import { ConfirmRequired, type Effect, useApiErrorMessage } from '@/api/errors'
import { type Service, type ServiceType, useServiceTypes, useServices } from '@/api/services'
import { PageHeader } from '@/components/AppShell'
import { CategoryDot } from '@/components/CategoryDot'
import { ConfirmDialog, EffectList } from '@/components/ConfirmDialog'
import { SelectField } from '@/components/MultiSelect'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Switch } from '@/components/ui/switch'
import { formatDateTime } from '@/lib/format'
import { useUrlState } from '@/lib/urlState'
import { cn } from '@/lib/utils'

import {
  FollowCapForm,
  MusicRelevanceEditor,
  RelevanceEditor,
  SourceSettingsForm,
} from './CategoryForms'
import { RoleLibraries } from './LibraryPicker'

const SECTIONS = ['services', 'discover', 'source', 'follows'] as const
type Section = (typeof SECTIONS)[number]

const CAPABILITY: Record<RoleKind, string> = {
  catalog: 'metadata',
  source: 'requests',
  consume: 'libraries',
}

export default function CategoriesPage() {
  const { t } = useTranslation(['admin', 'common'])
  const categories = useCategories()
  const navigate = useNavigate()
  const list = categories.data ?? []
  // The category is part of the path: /admin/media/categories/:category (spec 0008 Q22).
  const current = (useParams().category as CategoryName | undefined) ?? 'movies'
  const selected = list.find((c) => c.category === current) ?? list[0]

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('categories.title')}
        description={t('categories.description')}
      />
      {categories.isPending && <p className="text-text-3">{t('common:status.loading')}</p>}
      {categories.isError && (
        <p role="alert" className="text-bad">
          {t('categories.errors.unavailable')}
        </p>
      )}
      {list.length > 0 && (
        <div
          role="tablist"
          aria-label={t('categories.tabs')}
          className="mb-6 inline-flex flex-wrap rounded-full border border-line bg-night-1 p-0.5"
        >
          {list.map((c) => (
            <button
              key={c.category}
              type="button"
              role="tab"
              aria-selected={c.category === selected?.category}
              onClick={() => navigate(`/admin/media/categories/${c.category}`, { replace: true })}
              className={cn(
                'inline-flex cursor-pointer items-center gap-2 rounded-full px-3.5 py-1 text-sm text-text-2 transition-colors hover:text-text max-lg:min-h-11',
                c.category === selected?.category && 'bg-night-2 text-text',
              )}
            >
              <CategoryDot category={c.category} />
              {t(`common:categories.${c.category}`)}
            </button>
          ))}
        </div>
      )}
      {selected && <CategoryPanel key={selected.category} settings={selected} />}
    </>
  )
}

function CategoryPanel({ settings }: { settings: CategorySettings }) {
  const { t } = useTranslation(['admin', 'common'])
  const errorMessage = useApiErrorMessage()
  const category = settings.category
  const kind = mediaKindOf(category)
  const setEnabled = useSetCategoryEnabled(category)
  const defaultCatalog = settings.catalogs.find((c) => c.isDefault)
  const name = t(`common:categories.${category}`)
  // Adult switches on only with the operator statement (spec 0007 Q12).
  const [statement, setStatement] = useState(false)
  const [turningOff, setTurningOff] = useState(false)
  const sections = SECTIONS.filter(
    (value) =>
      value === 'services' ||
      (value === 'discover' && Boolean(kind)) ||
      (value === 'source' && settings.sourceFlow) ||
      (value === 'follows' && defaultCatalog?.followCapPerWeek != null),
  )
  const [chosen, setSection] = useUrlState<Section>('section', z.enum(SECTIONS), 'services')
  const section = sections.includes(chosen) ? chosen : 'services'
  const statementMissing = category === 'adult' && settings.enabled && !settings.operatorStatement
  const switchTo = async (enabled: boolean, operatorStatement = false) => {
    try {
      await setEnabled.mutateAsync({ enabled, operatorStatement })
      setStatement(false)
    } catch (err) {
      toast.error(errorMessage(err))
    }
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center gap-x-6 gap-y-3 rounded-panel border border-line bg-night-1 px-5 py-4">
        <label className="flex cursor-pointer items-center gap-3 text-base font-medium text-text">
          <Switch
            checked={settings.enabled}
            disabled={setEnabled.isPending}
            onCheckedChange={(enabled) => {
              if (enabled && category === 'adult') setStatement(true)
              else if (enabled) void switchTo(true)
              // Switching off closes the category for everybody: ask first (ADM-07).
              else setTurningOff(true)
            }}
          />
          {t('categories.enabled', { name })}
        </label>
        <div className="flex flex-wrap items-center gap-2">
          <DiscoverState settings={settings} />
          {settings.requestFlow && settings.enabled && !settings.requestsReady && (
            <StatusChip tone="warn">{t('categories.requests.closed')}</StatusChip>
          )}
          {statementMissing && (
            <StatusChip tone="warn">{t('categories.statement.missing')}</StatusChip>
          )}
        </div>
        {category === 'adult' && (
          <p className="basis-full text-sm text-text-3">{t('categories.adultOptIn')}</p>
        )}
        {statementMissing && (
          <div className="flex basis-full flex-wrap items-center gap-3">
            <p className="text-sm text-text-2">{t('categories.statement.missingHint')}</p>
            <Button size="sm" onClick={() => setStatement(true)}>
              {t('categories.statement.confirmNow')}
            </Button>
          </div>
        )}
        {category === 'adult' && settings.operatorStatement && (
          <p className="basis-full text-xs text-text-3">
            {t('categories.statement.given', {
              by: settings.operatorStatement.by,
              at: formatDateTime(settings.operatorStatement.at),
            })}
          </p>
        )}
      </div>
      <ConfirmDialog
        open={turningOff}
        onOpenChange={setTurningOff}
        title={t('categories.offConfirm.title', { name })}
        description={t(
          category === 'adult' ? 'categories.offConfirm.textAdult' : 'categories.offConfirm.text',
          { name },
        )}
        confirmLabel={t('categories.offConfirm.confirm')}
        pending={setEnabled.isPending}
        onConfirm={() => {
          setTurningOff(false)
          void switchTo(false)
        }}
      />
      {category === 'adult' && (
        <OperatorStatementDialog
          open={statement}
          pending={setEnabled.isPending}
          onOpenChange={setStatement}
          onConfirm={() => void switchTo(true, true)}
        />
      )}

      {category === 'music' && settings.requestFlow && (
        <p role="note" className="text-sm text-text-3">
          {t('categories.musicAvailability')}
        </p>
      )}

      {/* One section at a time instead of one long page (spec 0008 Q22, ADM-18). */}
      <div
        role="tablist"
        aria-label={t('categories.sections.label', { name })}
        className="inline-flex flex-wrap self-start rounded-full border border-line bg-night-1 p-0.5"
      >
        {sections.map((value) => (
          <button
            key={value}
            type="button"
            role="tab"
            aria-selected={section === value}
            onClick={() => setSection(value)}
            className={cn(
              'cursor-pointer rounded-full px-3.5 py-1 text-sm text-text-2 transition-colors hover:text-text max-lg:min-h-11',
              section === value && 'bg-night-2 text-text',
            )}
          >
            {t(`categories.sections.${value}`)}
          </button>
        ))}
      </div>
      {section === 'services' && (
        <Panel title={t('categories.roles.title')} description={t('categories.roles.description')}>
          <RolesEditor settings={settings} />
        </Panel>
      )}

      {section === 'discover' && kind && (
        <Panel
          title={t('categories.relevance.title')}
          description={t('categories.relevance.description')}
        >
          {defaultCatalog?.adapterType === 'musicbrainz' ? (
            <MusicRelevanceEditor
              key={defaultCatalog.id}
              category={category}
              catalog={defaultCatalog}
              defaults={settings.defaultRelevance}
            />
          ) : defaultCatalog ? (
            <RelevanceEditor
              key={defaultCatalog.id}
              category={category}
              kind={kind}
              catalog={defaultCatalog}
              defaults={settings.defaultRelevance}
            />
          ) : (
            <p className="text-text-3">{t('categories.relevance.needsCatalog')}</p>
          )}
        </Panel>
      )}

      {section === 'follows' && defaultCatalog?.followCapPerWeek != null && (
        <Panel
          title={t('categories.followCap.title')}
          description={t('categories.followCap.description')}
        >
          <FollowCapForm
            key={`${defaultCatalog.id}-${defaultCatalog.followCapPerWeek}`}
            category={category}
            catalog={{ ...defaultCatalog, followCapPerWeek: defaultCatalog.followCapPerWeek }}
          />
        </Panel>
      )}

      {section === 'source' && settings.sourceFlow && (
        <Panel
          title={t('categories.source.settingsTitle')}
          description={t('categories.source.settingsDescription')}
        >
          {settings.source ? (
            <SourceSettingsForm
              key={settings.source.id}
              category={category}
              source={settings.source}
              requestFlow={settings.requestFlow}
              requestsReady={settings.requestsReady}
            />
          ) : (
            <p className="text-text-3">{t('categories.source.needsSource')}</p>
          )}
        </Panel>
      )}
    </div>
  )
}

function DiscoverState({ settings }: { settings: CategorySettings }) {
  const { t } = useTranslation('admin')
  if (!settings.catalogFlow) return null
  if (settings.discoverReady) {
    return <StatusChip tone="ok">{t('categories.discover.ready')}</StatusChip>
  }
  const missing = !settings.enabled
    ? t('categories.discover.off')
    : settings.catalogs.length === 0
      ? t('categories.discover.noCatalog')
      : t('categories.discover.noRelevance')
  return <StatusChip tone="warn">{missing}</StatusChip>
}

function Panel({
  title,
  description,
  children,
}: {
  title: string
  description: string
  children: ReactNode
}) {
  return (
    <section className="rounded-panel border border-line bg-night-1">
      <header className="border-b border-line px-5 py-4">
        <h2 className="type-display text-lg font-bold text-text">{title}</h2>
        <p className="mt-0.5 text-sm text-text-2">{description}</p>
      </header>
      <div className="px-5 py-5">{children}</div>
    </section>
  )
}

/** Services that may take `kind` in `category`, not already in that role. */
function candidates(
  services: Service[],
  types: ServiceType[],
  category: CategoryName,
  kind: RoleKind,
  taken: number[],
) {
  return services.filter((s) => {
    const type = types.find((ty) => ty.typeId === s.adapterType)
    return (
      type !== undefined &&
      type.capabilities.includes(CAPABILITY[kind]) &&
      type.categories.includes(category) &&
      !taken.includes(s.id)
    )
  })
}

function RolesEditor({ settings }: { settings: CategorySettings }) {
  const { t } = useTranslation(['admin', 'common'])
  const errorMessage = useApiErrorMessage()
  const services = useServices().data ?? []
  const types = useServiceTypes().data ?? []
  const category = settings.category
  const addRole = useAddRole(category)
  const removeRole = useRemoveRole(category)
  const setDefault = useSetDefaultCatalog(category)
  const [switching, setSwitching] = useState(false)
  // Removing the source asks first, naming what the server says happens (ADM-02).
  const [removal, setRemoval] = useState<Effect[] | null>(null)
  // A catalog or a server to watch in: settings that go with it are named first (slice 12).
  const [roleRemoval, setRoleRemoval] = useState<{
    id: number
    name: string
    kind: 'catalog' | 'consume'
  } | null>(null)
  const categoryName = t(`common:categories.${category}`)
  const typeName = (id: string) => types.find((ty) => ty.typeId === id)?.displayName ?? id
  const run = async (action: () => Promise<unknown>) => {
    try {
      await action()
    } catch (err) {
      toast.error(errorMessage(err))
    }
  }
  const pick = (kind: RoleKind, taken: number[]) =>
    candidates(services, types, category, kind, taken)
  const source = settings.source
  const sourcePaused = services.some((s) => s.id === source?.serviceId && s.writesPausedAt != null)

  return (
    <div className="flex flex-col divide-y divide-line">
      {settings.catalogFlow && (
        <RoleRow label={t('categories.roles.catalog')} hint={t('categories.roles.catalogHint')}>
          {settings.catalogs.map((c) => (
            <RoleItem
              key={c.id}
              name={c.serviceName}
              type={typeName(c.adapterType)}
              badge={
                c.isDefault ? (
                  <StatusChip tone="neutral">{t('categories.roles.default')}</StatusChip>
                ) : (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => run(() => setDefault.mutateAsync(c.id))}
                  >
                    <StarIcon aria-hidden />
                    {t('categories.roles.makeDefault')}
                  </Button>
                )
              }
              onRemove={() => setRoleRemoval({ id: c.id, name: c.serviceName, kind: 'catalog' })}
            />
          ))}
          <AddRole
            kind="catalog"
            hasItems={settings.catalogs.length > 0}
            options={pick(
              'catalog',
              settings.catalogs.map((c) => c.serviceId),
            )}
            onAdd={(serviceId) => run(() => addRole.mutateAsync({ kind: 'catalog', serviceId }))}
          />
        </RoleRow>
      )}
      {settings.sourceFlow && (
        <RoleRow label={t('categories.roles.source')} hint={t('categories.roles.sourceHint')}>
          {source ? (
            <RoleItem
              name={source.serviceName}
              type={typeName(source.adapterType)}
              badge={
                <>
                  {sourcePaused && <StatusChip tone="warn">{t('services.writes.chip')}</StatusChip>}
                  {/* Nothing to switch to: disabled, with the reason (ADM-18). */}
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={pick('source', [source.serviceId]).length === 0}
                    title={
                      pick('source', [source.serviceId]).length === 0
                        ? t('categories.roles.noCandidates.source')
                        : undefined
                    }
                    onClick={() => setSwitching(true)}
                  >
                    <ArrowLeftRightIcon aria-hidden />
                    {t('categories.roles.switch')}
                  </Button>
                </>
              }
              note={
                sourcePaused && (
                  <p role="note" className="flex items-start gap-2 text-sm text-warn">
                    <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
                    {t('categories.roles.writesPaused')}
                  </p>
                )
              }
              onRemove={() =>
                run(async () => {
                  try {
                    await removeRole.mutateAsync({ id: source.id })
                    toast(t('categories.removeSource.done', { name: source.serviceName }))
                  } catch (error) {
                    if (!(error instanceof ConfirmRequired)) throw error
                    setRemoval(error.effects)
                  }
                })
              }
            />
          ) : (
            <AddRole
              kind="source"
              options={pick('source', [])}
              onAdd={(serviceId) => run(() => addRole.mutateAsync({ kind: 'source', serviceId }))}
            />
          )}
        </RoleRow>
      )}
      <RoleRow label={t('categories.roles.consume')} hint={t('categories.roles.consumeHint')}>
        {settings.consumers.map((c) => (
          <RoleItem
            key={c.id}
            name={c.serviceName}
            type={typeName(c.adapterType)}
            note={
              <div className="flex flex-col gap-2">
                <RoleLibraries category={category} consumer={c} />
                {services.some((s) => s.id === c.serviceId && !s.publicUrl) && (
                  <p role="note" className="flex items-start gap-2 text-sm text-warn">
                    <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
                    {t('categories.roles.noPublicUrl')}
                  </p>
                )}
              </div>
            }
            onRemove={() => setRoleRemoval({ id: c.id, name: c.serviceName, kind: 'consume' })}
          />
        ))}
        <AddRole
          kind="consume"
          hasItems={settings.consumers.length > 0}
          options={pick(
            'consume',
            settings.consumers.map((c) => c.serviceId),
          )}
          onAdd={(serviceId) => run(() => addRole.mutateAsync({ kind: 'consume', serviceId }))}
        />
      </RoleRow>
      <ConfirmDialog
        open={roleRemoval !== null}
        onOpenChange={(open) => !open && setRoleRemoval(null)}
        title={
          roleRemoval
            ? t(`categories.removeRole.title_${roleRemoval.kind}`, {
                name: roleRemoval.name,
                category: categoryName,
              })
            : ''
        }
        confirmLabel={t('categories.removeRole.confirm')}
        pending={removeRole.isPending}
        onConfirm={() =>
          roleRemoval &&
          run(async () => {
            await removeRole.mutateAsync({ id: roleRemoval.id })
            toast(
              t('categories.removeRole.done', { name: roleRemoval.name, category: categoryName }),
            )
            setRoleRemoval(null)
          })
        }
      >
        {roleRemoval?.kind === 'catalog' && (
          <>
            <p className="text-sm text-text-2">{t('categories.removeRole.catalogSettings')}</p>
            <p className="text-sm text-text-2">
              {settings.catalogs.length === 1
                ? t('categories.removeRole.lastCatalog', { category: categoryName })
                : t('categories.removeRole.otherCatalogs')}
            </p>
          </>
        )}
        {roleRemoval?.kind === 'consume' && (
          <p className="text-sm text-text-2">
            {t('categories.removeRole.consume', {
              name: roleRemoval.name,
              category: categoryName,
            })}
          </p>
        )}
      </ConfirmDialog>
      {source && (
        <ConfirmDialog
          open={removal !== null}
          onOpenChange={(open) => !open && setRemoval(null)}
          title={t('categories.removeSource.title', {
            name: source.serviceName,
            category: t(`common:categories.${category}`),
          })}
          confirmLabel={t('categories.removeSource.confirm')}
          pending={removeRole.isPending}
          onConfirm={() =>
            run(async () => {
              await removeRole.mutateAsync({ id: source.id, confirm: true })
              setRemoval(null)
              toast(t('categories.removeSource.done', { name: source.serviceName }))
            })
          }
        >
          <EffectList effects={removal ?? []} service={source.serviceName} />
          <p className="text-sm text-text-3">{t('categories.removeSource.switchHint')}</p>
        </ConfirmDialog>
      )}
      {source && (
        <SwitchSourceDialog
          open={switching}
          onOpenChange={setSwitching}
          category={category}
          options={pick('source', [source.serviceId])}
        />
      )}
    </div>
  )
}

function RoleRow({ label, hint, children }: { label: string; hint: string; children: ReactNode }) {
  return (
    <div className="grid gap-3 py-4 first:pt-0 last:pb-0 md:grid-cols-[12rem_minmax(0,1fr)]">
      <div>
        <h3 className="text-base font-semibold text-text">{label}</h3>
        <p className="text-sm text-text-3">{hint}</p>
      </div>
      <div className="flex flex-col gap-2">{children}</div>
    </div>
  )
}

function RoleItem({
  name,
  type,
  badge,
  note,
  onRemove,
}: {
  name: string
  type: string
  badge?: ReactNode
  /** Shown under the row, e.g. a missing setting. */
  note?: ReactNode
  onRemove: () => void
}) {
  const { t } = useTranslation('admin')
  return (
    <div className="flex flex-wrap items-center gap-3 rounded-card border border-line bg-night-0 px-3 py-2">
      <div className="min-w-0">
        <span className="font-medium text-text">{name}</span>
        <span className="ml-2 text-sm text-text-3">{type}</span>
      </div>
      <div className="ml-auto flex items-center gap-1">
        {badge}
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('categories.roles.remove', { name })}
          title={t('categories.roles.removeShort')}
          className="hover:text-bad"
          onClick={onRemove}
        >
          <Trash2Icon aria-hidden />
        </Button>
      </div>
      {note && <div className="basis-full">{note}</div>}
    </div>
  )
}

function AddRole({
  kind,
  options,
  onAdd,
  hasItems = false,
}: {
  kind: RoleKind
  options: Service[]
  onAdd: (serviceId: number) => void
  /** The role already has services: "none connected" would contradict them (ADM-18). */
  hasItems?: boolean
}) {
  const { t } = useTranslation('admin')
  const [choice, setChoice] = useState('')
  if (options.length === 0) {
    return hasItems ? null : (
      <p className="text-sm text-text-3">{t(`categories.roles.noCandidates.${kind}`)}</p>
    )
  }
  return (
    <div className="flex flex-wrap items-end gap-2">
      <div className="min-w-56 flex-1">
        <SelectField label={t(`categories.roles.add.${kind}`)} value={choice} onChange={setChoice}>
          <option value="">{t('categories.roles.choose')}</option>
          {options.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </SelectField>
      </div>
      <Button
        disabled={choice === ''}
        onClick={() => {
          onAdd(Number(choice))
          setChoice('')
        }}
      >
        <PlusIcon aria-hidden />
        {t('categories.roles.addButton')}
      </Button>
    </div>
  )
}

/** Pick another source, see what switching does, confirm. */
function SwitchSourceDialog({
  open,
  onOpenChange,
  category,
  options,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  category: CategoryName
  options: Service[]
}) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const setSource = useSetSource(category)
  const [choice, setChoice] = useState('')
  const [preview, setPreview] = useState<SourceSwitchPreview | null>(null)
  const [failure, setFailure] = useState<string | null>(null)
  const close = (next: boolean) => {
    if (!next) {
      setChoice('')
      setPreview(null)
      setFailure(null)
    }
    onOpenChange(next)
  }
  const choose = async (value: string) => {
    setChoice(value)
    setPreview(null)
    setFailure(null)
    if (!value) return
    try {
      setPreview(await fetchSourceSwitchPreview(category, Number(value)))
    } catch (err) {
      setFailure(errorMessage(err))
    }
  }
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('categories.switch.title')}</DialogTitle>
          <DialogDescription>{t('categories.switch.description')}</DialogDescription>
        </DialogHeader>
        <DialogBody>
          {options.length === 0 ? (
            <p className="text-text-3">{t('categories.roles.noCandidates.source')}</p>
          ) : (
            <SelectField label={t('categories.switch.to')} value={choice} onChange={choose}>
              <option value="">{t('categories.roles.choose')}</option>
              {options.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </SelectField>
          )}
          {preview && (
            <div className="rounded-card border border-line bg-night-0 px-4 py-3 text-sm text-text-2">
              <p>
                {t('categories.switch.summary', {
                  from: preview.current?.serviceName ?? '–',
                  to: preview.nextServiceName,
                })}
              </p>
              <p className="mt-1 text-text-3">
                {preview.affectedRequests.length === 0
                  ? t('categories.switch.noOpenRequests')
                  : t('categories.switch.openRequests', { count: preview.affectedRequests.length })}
              </p>
            </div>
          )}
          {failure && (
            <p role="alert" className="text-sm text-bad">
              {failure}
            </p>
          )}
        </DialogBody>
        <DialogFooter>
          <Button variant="ghost" onClick={() => close(false)}>
            {t('categories.switch.cancel')}
          </Button>
          <Button
            variant="primary"
            disabled={!preview || setSource.isPending}
            onClick={async () => {
              try {
                await setSource.mutateAsync(Number(choice))
                close(false)
              } catch (err) {
                setFailure(errorMessage(err))
              }
            }}
          >
            {t('categories.switch.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/**
 * The operator statement (spec 0007 Q12 A): asked once when adult is switched on, again after
 * every switch-off. Members are never asked.
 */
export function OperatorStatementDialog({
  open,
  pending,
  onOpenChange,
  onConfirm,
}: {
  open: boolean
  pending: boolean
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
}) {
  const { t } = useTranslation('admin')
  const [agreed, setAgreed] = useState(false)
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setAgreed(false)
        onOpenChange(next)
      }}
    >
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('categories.statement.title')}</DialogTitle>
          <DialogDescription>{t('categories.statement.description')}</DialogDescription>
        </DialogHeader>
        <DialogBody>
          <ul className="flex list-disc flex-col gap-1.5 pl-5 text-sm text-text-2">
            <li>{t('categories.statement.legal')}</li>
            <li>{t('categories.statement.adults')}</li>
            <li>{t('categories.statement.responsible')}</li>
          </ul>
          <label className="flex cursor-pointer items-start gap-3 text-sm text-text">
            <input
              type="checkbox"
              checked={agreed}
              onChange={(e) => setAgreed(e.target.checked)}
              className="mt-0.5 size-4 accent-polaris"
            />
            {t('categories.statement.agree')}
          </label>
        </DialogBody>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {t('categories.statement.cancel')}
          </Button>
          <Button variant="primary" disabled={!agreed || pending} onClick={onConfirm}>
            {t('categories.statement.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
