import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery } from '@tanstack/react-query'
import { CircleAlertIcon, CircleCheckIcon, LoaderCircleIcon, TriangleAlertIcon } from 'lucide-react'
import { type ComponentProps, type ReactNode, useId, useRef, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

import { api } from '@/api/client'
import { useCategories } from '@/api/categories'
import { ApiError, ConfirmRequired, type Effect, unwrap } from '@/api/errors'
import { useJobRun, useRun } from '@/api/runs'
import {
  type Service,
  type ServiceType,
  type UpdateServiceInput,
  useCreateService,
  useDeleteService,
  useServiceTypes,
  useUpdateService,
} from '@/api/services'
import { ConfirmDialog, EffectList } from '@/components/ConfirmDialog'
import { FormField } from '@/components/FormField'
import { TechnicalDetails } from '@/components/TechnicalDetails'
import { ServiceStatusChip } from '@/components/ServiceStatusChip'
import { useScrollIntoDialogView } from '@/lib/dialogView'
import { ElevationCancelled, useElevation } from '@/lib/elevation'
import { formatDateTime, formatRelative } from '@/lib/format'
import {
  exampleUrl,
  sameOrigin,
  serviceDependencies,
  serviceErrorKey,
  serviceStatus,
} from '@/lib/services'
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
import { Label } from '@/components/ui/label'
import { required } from '@/lib/validation'
import { cn } from '@/lib/utils'

const baseUrl = z
  .string()
  .trim()
  .regex(/^https?:\/\/[^\s/?#]+/i, { message: 'url' })

/** Optional: empty, or an http(s) URL. */
const publicUrl = z
  .string()
  .trim()
  .refine((value) => value === '' || /^https?:\/\/[^\s/?#]+/i.test(value), { message: 'url' })

// The API key is required only for types that take one (`needsCredential`), checked on submit.
const addSchema = z.object({
  adapterType: required,
  name: required,
  baseUrl,
  apiKey: z.string().trim(),
  username: z.string().trim(),
  password: z.string(),
  publicUrl,
})

const editSchema = z.object({
  adapterType: z.string(),
  name: required,
  baseUrl,
  apiKey: z.string().trim(),
  username: z.string().trim(),
  password: z.string(),
  publicUrl,
})

type FormValues = z.infer<typeof addSchema>

/**
 * The add form checks everything at once (ADM-20): the key or login the chosen type takes is
 * required here too, so all missing fields show together.
 */
function addSchemaFor(types: ServiceType[]) {
  return addSchema.superRefine((values, ctx) => {
    const login = takesLogin(types, values.adapterType)
    if (!login && needsCredential(types, values.adapterType) && values.apiKey === '') {
      ctx.addIssue({ code: 'custom', path: ['apiKey'], message: 'required' })
    }
    if (login && values.username === '') {
      ctx.addIssue({ code: 'custom', path: ['username'], message: 'required' })
    }
    if (login && values.password === '') {
      ctx.addIssue({ code: 'custom', path: ['password'], message: 'required' })
    }
  })
}

/** Public services (TMDB, MusicBrainz) have a fixed address the form fills in. */
function defaultUrl(types: ServiceType[], typeId: string): string | null {
  return types.find((ty) => ty.typeId === typeId)?.defaultBaseUrl ?? null
}

function needsCredential(types: ServiceType[], typeId: string): boolean {
  return types.find((ty) => ty.typeId === typeId)?.needsCredential ?? true
}

/** Services without an API key take an admin's login (Navidrome). */
function takesLogin(types: ServiceType[], typeId: string): boolean {
  return types.find((ty) => ty.typeId === typeId)?.credential === 'login'
}

/** Services whose libraries Starr reads link members to their web client. */
function buildsLinks(types: ServiceType[], typeId: string): boolean {
  return types.find((ty) => ty.typeId === typeId)?.capabilities.includes('libraries') ?? false
}

type Phase =
  | { kind: 'form' }
  // `previousUrl`: the address before this save, offered back if the new one fails (ADM-05).
  | { kind: 'check'; jobId: number; previousUrl?: string }
  // The server named consequences of the change (spec 0008 ADM-01); nothing is saved yet.
  | { kind: 'confirm'; effects: Effect[]; body: UpdateServiceInput }

/**
 * Add a service, or edit one. After saving a connection change the dialog
 * follows the queued check until it finished and shows the result.
 */
export function ServiceFormDialog({
  service,
  open,
  onOpenChange,
}: {
  /** Edit this service; absent to add a new one. */
  service?: Service
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const types = useServiceTypes()
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className={cn('max-w-lg', service && 'lg:max-w-3xl')}>
        {/* Mounted per opening, so every opening starts from fresh state. */}
        {open && types.data && (
          <ServiceForm
            key={service?.id ?? 'new'}
            service={service}
            types={types.data}
            onClose={() => onOpenChange(false)}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function ServiceForm({
  service,
  types,
  onClose,
}: {
  service?: Service
  types: ServiceType[]
  onClose: () => void
}) {
  const { t } = useTranslation('admin')
  // After "add", the dialog continues with the created service.
  const [target, setTarget] = useState<Service | undefined>(service)
  const [phase, setPhase] = useState<Phase>({ kind: 'form' })
  const [failure, setFailure] = useState<string | null>(null)
  const create = useCreateService()
  const update = useUpdateService()
  // A new URL or credential needs a fresh re-authentication (the server asks for it).
  const elevation = useElevation()
  const editing = target !== undefined

  const form = useForm<FormValues>({
    resolver: zodResolver(editing ? editSchema : addSchemaFor(types)),
    defaultValues: formDefaults(target, types),
  })

  const onSubmit = form.handleSubmit(async (values) => {
    setFailure(null)
    const links = buildsLinks(types, values.adapterType)
    const login = takesLogin(types, values.adapterType)
    const credential = needsCredential(types, values.adapterType) && !login
    // Another server (scheme, host or port): the stored key or login is not sent there.
    const toNewServer = target !== undefined && !sameOrigin(values.baseUrl, target.baseUrl)
    if ((!target || toNewServer) && credential && values.apiKey === '') {
      form.setError('apiKey', { type: 'required', message: target ? 'newServer' : 'required' })
      return
    }
    // A login is sent whole: both fields when adding, or both to change it.
    const loginGiven = values.username !== '' || values.password !== ''
    if (login && (!target || loginGiven || toNewServer)) {
      let missing = false
      const message = target && toNewServer && !loginGiven ? 'newServer' : 'required'
      if (values.username === '') {
        form.setError('username', { type: 'required', message })
        missing = true
      }
      if (values.password === '') {
        form.setError('password', { type: 'required', message })
        missing = true
      }
      if (missing) return
    }
    const loginFields =
      login && loginGiven ? { username: values.username, password: values.password } : {}
    try {
      if (!target) {
        const res = await create.mutateAsync({
          adapterType: values.adapterType,
          name: values.name,
          baseUrl: values.baseUrl,
          apiKey: credential ? values.apiKey : undefined,
          ...loginFields,
          publicUrl: links && values.publicUrl !== '' ? values.publicUrl : undefined,
        })
        setTarget(res.service)
        if (res.jobId != null) setPhase({ kind: 'check', jobId: res.jobId })
        return
      }
      await save(target, {
        name: values.name,
        baseUrl: values.baseUrl,
        apiKey: values.apiKey === '' || !credential ? undefined : values.apiKey,
        ...loginFields,
        publicUrl: links ? (values.publicUrl === '' ? null : values.publicUrl) : undefined,
      })
    } catch (error) {
      setFailure(error instanceof ApiError ? error.code : 'unavailable')
    }
  })

  /** Save a change; one with consequences asks first and is sent again with `confirm`. */
  const save = async (service: Service, body: UpdateServiceInput, reverting = false) => {
    try {
      const res = await elevation.run(() => update.mutateAsync({ id: service.id, body }))
      setTarget(res.service)
      if (res.jobId != null) {
        // Only an address change alone can be undone by its address (a new key stays new).
        const addressOnly = body.apiKey === undefined && body.username === undefined
        const previousUrl =
          !reverting && addressOnly && service.baseUrl !== res.service.baseUrl
            ? service.baseUrl
            : undefined
        setPhase({ kind: 'check', jobId: res.jobId, previousUrl })
      } else {
        onClose()
      }
    } catch (error) {
      // The admin closed the re-authentication: nothing was saved, the form stays.
      if (error instanceof ElevationCancelled) return
      if (!(error instanceof ConfirmRequired)) throw error
      setPhase({ kind: 'confirm', effects: error.effects, body })
    }
  }

  const typeName = (id: string) => types.find((ty) => ty.typeId === id)?.displayName ?? id
  const adapterType = useWatch({ control: form.control, name: 'adapterType' })
  const isTmdb = adapterType === 'tmdb'
  const isMusicBrainz = adapterType === 'musicbrainz'
  const isStashDb = adapterType === 'stashdb'
  const fixedUrl = defaultUrl(types, adapterType)
  const login = takesLogin(types, adapterType)
  const takesKey = needsCredential(types, adapterType) && !login
  const isLibrary = buildsLinks(types, adapterType)
  const publicUrlValue = useWatch({ control: form.control, name: 'publicUrl' })
  const baseUrlValue = useWatch({ control: form.control, name: 'baseUrl' })
  const newServer = target !== undefined && !sameOrigin(baseUrlValue ?? '', target.baseUrl)
  const title = target ? t('services.form.editTitle', { name: target.name }) : t('services.add')
  const description = target
    ? t('services.form.editDescription')
    : t('services.form.addDescription')

  return (
    <>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{description}</DialogDescription>
      </DialogHeader>

      {phase.kind === 'confirm' && target ? (
        <>
          <DialogBody>
            <div className="flex flex-col gap-3">
              <p className="flex items-start gap-2 text-base font-medium text-text">
                <TriangleAlertIcon aria-hidden className="mt-1 size-4 shrink-0 text-warn" />
                {t('services.confirmAddress.lead', { name: target.name })}
              </p>
              <EffectList effects={phase.effects} service={target.name} />
            </div>
            {failure && <ErrorBanner code={failure} />}
          </DialogBody>
          <DialogFooter>
            <Button variant="ghost" autoFocus onClick={() => setPhase({ kind: 'form' })}>
              {t('services.confirmAddress.back')}
            </Button>
            <Button
              className="border-bad/60 text-bad hover:border-bad"
              disabled={update.isPending}
              onClick={() => {
                setFailure(null)
                save(target, { ...phase.body, confirm: true }).catch((error: unknown) =>
                  setFailure(error instanceof ApiError ? error.code : 'unavailable'),
                )
              }}
            >
              {t('services.confirmAddress.confirm')}
            </Button>
          </DialogFooter>
        </>
      ) : phase.kind === 'check' && target ? (
        <>
          <DialogBody>
            {failure && <ErrorBanner code={failure} />}
            <CheckResult
              serviceId={target.id}
              jobId={phase.jobId}
              typeName={typeName(target.adapterType)}
              failedAction={
                phase.previousUrl && (
                  <Button
                    size="sm"
                    className="ml-6.5 self-start"
                    disabled={update.isPending}
                    onClick={() => {
                      setFailure(null)
                      save(target, { baseUrl: phase.previousUrl }, true).catch((error: unknown) =>
                        setFailure(error instanceof ApiError ? error.code : 'unavailable'),
                      )
                    }}
                  >
                    {t('services.check.revert', { url: phase.previousUrl })}
                  </Button>
                )
              }
            />
          </DialogBody>
          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => {
                form.reset(formDefaults(target, types))
                setPhase({ kind: 'form' })
              }}
            >
              {t('services.form.back')}
            </Button>
            <Button onClick={onClose}>{t('services.form.done')}</Button>
          </DialogFooter>
        </>
      ) : (
        <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
          {/* Two columns once the dialog is wide enough (editing at ≥ 1024 px): the facts
              beside the connection fields. They come first for screen readers either way. */}
          <DialogBody className="@container">
            <div className="flex flex-col gap-5 @2xl:grid @2xl:grid-cols-[minmax(0,1fr)_minmax(0,20rem)] @2xl:items-start">
              {target && (
                <ServiceFacts
                  service={target}
                  type={types.find((ty) => ty.typeId === target.adapterType)}
                  className="@2xl:sticky @2xl:top-0 @2xl:col-start-2 @2xl:row-start-1"
                />
              )}
              <div className="flex flex-col gap-5 @2xl:col-start-1 @2xl:row-start-1">
                {failure && <ErrorBanner code={failure} />}
                <TypeSelect
                  types={types}
                  disabled={editing}
                  {...form.register('adapterType', {
                    onChange: (event: { target: { value: string } }) => {
                      // The name follows the type until the admin typed their own (ADM-20).
                      const name = form.getValues('name').trim()
                      if (name === '' || types.some((ty) => ty.displayName === name)) {
                        form.setValue('name', typeName(event.target.value))
                      }
                      const current = form.getValues('baseUrl').trim()
                      const next = defaultUrl(types, event.target.value)
                      const isDefault = types.some((ty) => ty.defaultBaseUrl === current)
                      if (current === '' || isDefault) form.setValue('baseUrl', next ?? '')
                    },
                  })}
                />
                <FormField
                  label={t('services.form.name')}
                  hint={t('services.form.nameHint')}
                  error={form.formState.errors.name?.message}
                  autoComplete="off"
                  {...form.register('name')}
                />
                <FormField
                  label={t('services.form.baseUrl')}
                  hint={
                    isTmdb
                      ? t('services.form.baseUrlHintTmdb')
                      : isMusicBrainz
                        ? t('services.form.baseUrlHintMusicBrainz')
                        : isStashDb
                          ? t('services.form.baseUrlHintStashDb')
                          : t('services.form.baseUrlHint', { example: exampleUrl(adapterType) })
                  }
                  error={form.formState.errors.baseUrl?.message}
                  placeholder={exampleUrl(adapterType, fixedUrl)}
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  inputMode="url"
                  className="type-data"
                  {...form.register('baseUrl')}
                />
                {!takesKey && !login && (
                  <p role="note" className="text-sm text-text-2">
                    {t('services.form.noKeyMusicBrainz')}
                  </p>
                )}
                {login && (
                  <>
                    <FormField
                      label={t('services.form.username')}
                      hint={
                        newServer
                          ? t('services.form.loginNewServer')
                          : editing
                            ? t('services.form.loginKeep')
                            : t('services.form.usernameHint')
                      }
                      error={form.formState.errors.username?.message}
                      autoComplete="off"
                      autoCapitalize="none"
                      spellCheck={false}
                      {...form.register('username')}
                    />
                    <FormField
                      label={t('services.form.password')}
                      hint={t('services.form.passwordHint')}
                      error={form.formState.errors.password?.message}
                      type="password"
                      autoComplete="new-password"
                      spellCheck={false}
                      {...form.register('password')}
                    />
                  </>
                )}
                {takesKey && (
                  <FormField
                    label={isTmdb ? t('services.form.apiKeyTmdb') : t('services.form.apiKey')}
                    hint={
                      newServer
                        ? t('services.form.apiKeyNewServer')
                        : editing
                          ? t('services.form.apiKeyKeep')
                          : isTmdb
                            ? t('services.form.apiKeyHintTmdb')
                            : isStashDb
                              ? t('services.form.apiKeyHintStashDb')
                              : isLibrary
                                ? t('services.form.apiKeyHintLibrary')
                                : t('services.form.apiKeyHint')
                    }
                    error={form.formState.errors.apiKey?.message}
                    type="password"
                    autoComplete="off"
                    spellCheck={false}
                    className="type-data"
                    {...form.register('apiKey')}
                  />
                )}
                {isLibrary && (
                  <FormField
                    label={t('services.form.publicUrl')}
                    hint={t('services.form.publicUrlHint')}
                    error={form.formState.errors.publicUrl?.message}
                    placeholder="https://watch.example.com"
                    autoComplete="off"
                    autoCapitalize="none"
                    spellCheck={false}
                    inputMode="url"
                    className="type-data"
                    {...form.register('publicUrl')}
                  />
                )}
                {isLibrary && !publicUrlValue?.trim() && (
                  <p role="note" className="flex items-start gap-2 text-sm text-warn">
                    <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
                    {t('services.form.publicUrlMissing')}
                  </p>
                )}
              </div>
            </div>
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              {t('services.form.cancel')}
            </Button>
            <Button type="submit" variant="primary" disabled={form.formState.isSubmitting}>
              {editing ? t('services.form.submitSave') : t('services.form.submitAdd')}
            </Button>
          </DialogFooter>
        </form>
      )}
      {elevation.dialog}
    </>
  )
}

function formDefaults(service: Service | undefined, types: ServiceType[]): FormValues {
  return {
    adapterType: service?.adapterType ?? types[0]?.typeId ?? '',
    name: service?.name ?? types[0]?.displayName ?? '',
    baseUrl: service?.baseUrl ?? types[0]?.defaultBaseUrl ?? '',
    apiKey: '',
    username: '',
    password: '',
    publicUrl: service?.publicUrl ?? '',
  }
}

/** What Starr knows about a service from its last check. */
function ServiceFacts({
  service,
  type,
  className,
}: {
  service: Service
  type?: ServiceType
  className?: string
}) {
  const { t } = useTranslation('admin')
  return (
    <dl
      className={cn(
        'grid grid-cols-[max-content_1fr] items-center gap-x-6 gap-y-2 rounded-card border border-line bg-night-1 px-4 py-3 text-sm',
        className,
      )}
    >
      <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
        {t('services.columns.status')}
      </dt>
      <dd>
        <ServiceStatusChip service={service} />
      </dd>
      <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
        {t('services.columns.version')}
      </dt>
      <dd className="type-data text-text">
        {service.detectedVersion ?? '–'}
        {type && (
          <span className="block font-sans text-xs text-text-3">
            {t('services.detail.tested', {
              min: type.testedVersions.min,
              max: type.testedVersions.maxExclusive,
            })}
          </span>
        )}
      </dd>
      <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
        {t('services.detail.capabilities')}
      </dt>
      <dd className="text-text">
        {service.capabilities.length > 0
          ? service.capabilities
              .map((c) =>
                t(
                  `services.capability.${c as 'metadata' | 'requests' | 'accounts' | 'libraries'}`,
                  {
                    defaultValue: c,
                  },
                ),
              )
              .join(', ')
          : t('services.detail.noCapabilities')}
      </dd>
      <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
        {t('services.detail.mode')}
      </dt>
      <dd className="text-text">
        {service.mode === 'probe' ? t('services.detail.modeProbe') : t('services.detail.modeLive')}
      </dd>
      <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
        {t('services.columns.lastCheck')}
      </dt>
      <dd className="type-data text-text-2">
        {service.lastCheckAt ? formatRelative(service.lastCheckAt) : t('services.never')}
        <span className="ml-2 font-sans text-xs text-text-3">
          {t('services.detail.created')} {formatDateTime(service.createdAt)}
        </span>
      </dd>
    </dl>
  )
}

function TypeSelect({ types, ...props }: { types: ServiceType[] } & ComponentProps<'select'>) {
  const { t } = useTranslation('admin')
  const id = useId()
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{t('services.form.type')}</Label>
      <select
        id={id}
        className={cn(
          'h-10 w-full cursor-pointer rounded-control border border-line bg-night-0 px-3 text-base text-text transition-colors outline-hidden',
          'hover:border-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-0 focus-visible:outline-polaris/40',
          'disabled:cursor-not-allowed disabled:opacity-60',
        )}
        {...props}
      >
        {types.map((type) => (
          <option key={type.typeId} value={type.typeId}>
            {type.displayName}
          </option>
        ))}
      </select>
    </div>
  )
}

function ErrorBanner({ code }: { code: string }) {
  const { t } = useTranslation('admin')
  const ref = useRef<HTMLDivElement>(null)
  useScrollIntoDialogView(ref)
  return (
    <div
      ref={ref}
      role="alert"
      className="chip flex items-start gap-2.5 rounded-card px-4 py-3 text-base [--chip:var(--color-bad)]"
    >
      <CircleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-bad" />
      <span>{t(serviceErrorKey(code), { defaultValue: t('services.errors.unavailable') })}</span>
    </div>
  )
}

function useService(id: number, enabled: boolean) {
  return useQuery({
    queryKey: ['services', id],
    enabled,
    queryFn: async () => {
      const data = await unwrap(
        api.GET('/api/v1/services/{id}', {
          params: { path: { id } },
        }),
      )
      return data
    },
  })
}

/** Follows a queued check and shows its outcome. */
export function CheckResult({
  serviceId,
  jobId,
  typeName,
  failedAction,
}: {
  serviceId: number
  jobId: number
  typeName: string
  /** Offered when the check failed, e.g. going back to the address that worked (ADM-05). */
  failedAction?: ReactNode
}) {
  const { t } = useTranslation('admin')
  const run = useJobRun(jobId).data ?? null
  const finished = run !== null && run.state !== 'running'
  const service = useService(serviceId, finished).data
  const detail = useRun(finished && run ? run.id : null).data
  const item = detail?.report?.items?.[0]

  let tone: 'info' | 'ok' | 'warn' | 'bad' = 'info'
  let icon = (
    <LoaderCircleIcon aria-hidden className="size-4 shrink-0 motion-safe:animate-spin text-info" />
  )
  let message = run ? t('services.check.running') : t('services.check.queued')
  let extra: string | null = null

  if (finished && run && run.state !== 'succeeded') {
    tone = 'bad'
    icon = <CircleAlertIcon aria-hidden className="size-4 shrink-0 text-bad" />
    message = t('services.check.jobFailed', { error: run.error ?? run.state })
  } else if (finished && service) {
    const status = serviceStatus(service)
    if (status === 'ok') {
      tone = 'ok'
      icon = <CircleCheckIcon aria-hidden className="size-4 shrink-0 text-ok" />
      message = t('services.check.ok', { type: typeName, version: service.detectedVersion ?? '' })
    } else if (service.lastError) {
      tone = status === 'warning' ? 'warn' : 'bad'
      icon =
        status === 'warning' ? (
          <TriangleAlertIcon aria-hidden className="size-4 shrink-0 text-warn" />
        ) : (
          <CircleAlertIcon aria-hidden className="size-4 shrink-0 text-bad" />
        )
      message = t(serviceErrorKey(service.lastError), {
        defaultValue: t('services.errors.unknown'),
      })
      extra = item?.detail ?? null
    }
  }

  return (
    <div
      role="status"
      aria-live="polite"
      className={cn(
        'chip flex flex-col gap-2 rounded-card px-4 py-3 text-base',
        {
          info: '[--chip:var(--color-info)]',
          ok: '[--chip:var(--color-ok)]',
          warn: '[--chip:var(--color-warn)]',
          bad: '[--chip:var(--color-bad)]',
        }[tone],
      )}
    >
      <div className="flex items-start gap-2.5">
        <span className="mt-0.5">{icon}</span>
        <span>{message}</span>
      </div>
      {extra && <TechnicalDetails text={extra} className="pl-6.5" />}
      {(tone === 'bad' || tone === 'warn') && finished && failedAction}
      {run && finished && (
        <Link
          to={`/admin/system/runs?run=${run.id}`}
          className="pl-6.5 text-sm underline underline-offset-2 opacity-90 hover:opacity-100"
        >
          {t('services.check.viewRun')}
        </Link>
      )}
    </div>
  )
}

/**
 * Removing a service (ADM-21): names every category role it plays and, while it plays one, asks
 * for its name to be typed.
 */
export function DeleteServiceDialog({
  service,
  onOpenChange,
}: {
  service: Service | null
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useTranslation(['admin', 'common'])
  const remove = useDeleteService()
  const categories = useCategories()
  const [failure, setFailure] = useState<string | null>(null)
  const dependencies = service ? serviceDependencies(categories.data ?? [], service.id) : []

  const close = (open: boolean) => {
    if (!open) setFailure(null)
    onOpenChange(open)
  }

  return (
    <ConfirmDialog
      open={service !== null}
      onOpenChange={close}
      title={t('services.delete.title', { name: service?.name ?? '' })}
      description={t('services.delete.description')}
      confirmLabel={t('services.delete.confirm')}
      // Unknown dependencies (categories not loaded) count as "in use".
      confirmPhrase={dependencies.length > 0 || categories.isError ? service?.name : undefined}
      pending={remove.isPending || categories.isPending}
      onConfirm={async () => {
        if (!service) return
        try {
          await remove.mutateAsync(service.id)
          close(false)
          toast(t('services.delete.done', { name: service.name }))
        } catch (error) {
          setFailure(error instanceof ApiError ? error.code : 'unavailable')
        }
      }}
    >
      {dependencies.length > 0 && (
        <div className="flex flex-col gap-2">
          <p className="text-sm font-medium text-text">{t('services.delete.inUse')}</p>
          <ul className="flex list-disc flex-col gap-1 pl-5 text-sm text-text-2 marker:text-text-3">
            {dependencies.map((d) => (
              <li key={`${d.category}-${d.role}`}>
                {t(`services.delete.role.${d.role}`, {
                  category: t(`common:categories.${d.category}`),
                })}
              </li>
            ))}
          </ul>
        </div>
      )}
      {categories.isError && (
        <p className="text-sm text-text-2">{t('services.delete.unknownUse')}</p>
      )}
      {failure && <ErrorBanner code={failure} />}
    </ConfirmDialog>
  )
}
