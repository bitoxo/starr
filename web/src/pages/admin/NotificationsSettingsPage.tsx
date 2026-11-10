import { zodResolver } from '@hookform/resolvers/zod'
import { MailIcon, PlusIcon, SendIcon, Trash2Icon, WebhookIcon } from 'lucide-react'
import {
  type ComponentProps,
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import {
  ADMIN_KINDS,
  type AdminKind,
  type Channel,
  type SmtpSecurity,
  useChannels,
  useCreateChannel,
  useDeleteChannel,
  useRefreshChannels,
  useTestChannel,
  useUpdateChannel,
} from '@/api/notifications'
import { useJobRun } from '@/api/runs'
import { PageHeader } from '@/components/AppShell'
import { EmptyState } from '@/components/EmptyState'
import { FormField } from '@/components/FormField'
import { ErrorNotice } from '@/components/members'
import { StatusChip } from '@/components/StatusChip'
import { TechnicalDetails } from '@/components/TechnicalDetails'
import { UnsavedChangesGuard } from '@/components/UnsavedChanges'
import { QueryError } from '@/components/QueryError'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
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
import { Switch } from '@/components/ui/switch'
import { ElevationCancelled, useElevation, useEnsureElevated } from '@/lib/elevation'
import { formatRelative } from '@/lib/format'
import { toggleKind } from '@/lib/notifications'
import { serviceErrorKey } from '@/lib/services'
import { cn } from '@/lib/utils'
import { email as emailField, required } from '@/lib/validation'

/** The admin kinds of a stored list (the server only stores those). */
function adminKinds(kinds: string[]): AdminKind[] {
  return ADMIN_KINDS.filter((k) => kinds.includes(k))
}

const SECURITY: SmtpSecurity[] = ['starttls', 'tls', 'none']
const DEFAULT_PORT: Record<SmtpSecurity, number> = { starttls: 587, tls: 465, none: 25 }

const optionalEmail = z
  .string()
  .trim()
  .refine((v) => v === '' || emailField.safeParse(v).success, { message: 'emailInvalid' })

const emailSchema = z.object({
  host: required,
  port: z.coerce.number<string>().int().min(1, { message: 'port' }).max(65535, { message: 'port' }),
  security: z.enum(['starttls', 'tls', 'none']),
  username: z.string().trim(),
  password: z.string(),
  fromAddress: emailField,
  fromName: z.string().trim(),
  adminAddress: optionalEmail,
})
type EmailForm = z.input<typeof emailSchema>

const webhookSchema = z.object({
  kind: z.enum(['discord', 'webhook']),
  name: required,
  url: z
    .string()
    .trim()
    .regex(/^https?:\/\/[^\s/?#]+\/\S*$/i, { message: 'webhookUrl' }),
})
type WebhookForm = z.input<typeof webhookSchema>

export default function NotificationsSettingsPage() {
  const { t } = useTranslation('admin')
  const channels = useChannels()
  const errorMessage = useApiErrorMessage()
  const email = channels.data?.find((c) => c.kind === 'email')
  const hooks = channels.data?.filter((c) => c.kind !== 'email') ?? []
  // One save model (ADM-26): every channel saves with its own button; leaving with unsaved
  // changes in any of them asks first (one guard per page).
  const [dirty, setDirty] = useState<ReadonlySet<string>>(new Set())
  const reportDirty = useCallback((key: string, isDirty: boolean) => {
    setDirty((current) => {
      if (current.has(key) === isDirty) return current
      const next = new Set(current)
      if (isDirty) next.add(key)
      else next.delete(key)
      return next
    })
  }, [])

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('notificationSettings.title')}
        description={t('notificationSettings.description')}
      />
      <UnsavedChangesGuard when={dirty.size > 0} />
      {channels.isError && (
        <QueryError
          message={errorMessage(channels.error)}
          onRetry={() => void channels.refetch()}
        />
      )}
      {channels.data && (
        <div className="flex max-w-3xl flex-col gap-10">
          <Section
            icon={MailIcon}
            title={t('notificationSettings.email.title')}
            description={t('notificationSettings.email.description')}
          >
            <EmailCard key={email?.id ?? 'new'} channel={email} onDirty={reportDirty} />
          </Section>
          <Section
            icon={WebhookIcon}
            title={t('notificationSettings.hooks.title')}
            description={t('notificationSettings.hooks.description')}
            action={<AddWebhook />}
          >
            {hooks.length === 0 ? (
              <EmptyState
                icon={WebhookIcon}
                title={t('notificationSettings.hooks.emptyTitle')}
                description={t('notificationSettings.hooks.emptyDescription')}
              />
            ) : (
              <ul className="flex flex-col divide-y divide-line rounded-card border border-line bg-night-1">
                {hooks.map((hook) => (
                  <WebhookRow key={hook.id} channel={hook} onDirty={reportDirty} />
                ))}
              </ul>
            )}
          </Section>
        </div>
      )}
    </>
  )
}

function Section({
  icon: Icon,
  title,
  description,
  action,
  children,
}: {
  icon: typeof MailIcon
  title: string
  description: string
  action?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="flex flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div className="flex flex-col gap-1">
          <h2 className="flex items-center gap-2 text-lg font-semibold text-text">
            <Icon aria-hidden className="size-4 text-text-3" strokeWidth={1.75} />
            {title}
          </h2>
          <p className="text-sm text-text-2">{description}</p>
        </div>
        {action}
      </div>
      {children}
    </section>
  )
}

/** The reason of a failed delivery in words (ADM-17); the raw text goes behind a disclosure. */
function useFailureReason() {
  const { t } = useTranslation('admin')
  return (code: string | null | undefined) => {
    if (code === 'no_address' || code === 'settings_invalid') {
      return t(`notificationSettings.errors.${code}`)
    }
    const generic = t('notificationSettings.errors.generic')
    return code ? t(serviceErrorKey(code), { defaultValue: generic }) : generic
  }
}

/**
 * Outcome of the last delivery or test (ADM-26): only the outcome, the on/off state is the
 * switch's alone; on failure the reason in words and the technical text collapsed, shown also
 * while the channel is off so a failed test is never "shown on the channel" without a trace.
 */
function ChannelStatus({ channel }: { channel: Channel }) {
  const { t } = useTranslation('admin')
  const reason = useFailureReason()
  if (channel.lastOk == null || !channel.lastAttemptAt) {
    return (
      <StatusChip tone="info" className="self-start">
        {t('notificationSettings.status.untested')}
      </StatusChip>
    )
  }
  return (
    <span className="flex flex-col items-start gap-1">
      {channel.lastOk ? (
        <StatusChip tone="ok">
          {t('notificationSettings.status.ok', { when: formatRelative(channel.lastAttemptAt) })}
        </StatusChip>
      ) : (
        <StatusChip tone="bad">
          {t('notificationSettings.status.failed', { when: formatRelative(channel.lastAttemptAt) })}
        </StatusChip>
      )}
      {!channel.lastOk && (
        <>
          <span className="text-sm text-text-2">{reason(channel.lastErrorCode)}</span>
          {channel.lastError && <TechnicalDetails text={channel.lastError} />}
        </>
      )}
    </span>
  )
}

/** Queue a test and report its outcome once the worker ran it. */
function useChannelTest() {
  const { t } = useTranslation('admin')
  const test = useTestChannel()
  const refresh = useRefreshChannels()
  const errorMessage = useApiErrorMessage()
  const [jobId, setJobId] = useState<number | null>(null)
  const reported = useRef<number | null>(null)
  const run = useJobRun(jobId)
  const finished = run.data && run.data.state !== 'running' ? run.data : null

  // Report each finished test once (toast and fresh channel status).
  useEffect(() => {
    if (!finished || reported.current === finished.jobId) return
    reported.current = finished.jobId
    void refresh()
    if (finished.state === 'succeeded' && finished.expectation === 'met') {
      toast(t('notificationSettings.test.sent'))
    } else {
      toast.error(t('notificationSettings.test.failed'))
    }
  }, [finished, refresh, t])

  return {
    pending: test.isPending || (jobId !== null && !finished),
    start: (id: number) =>
      test.mutate(id, {
        onSuccess: (queued) => setJobId(queued.jobId),
        onError: (error) => toast.error(errorMessage(error)),
      }),
  }
}

function EventChoice({
  value,
  onChange,
  disabled,
}: {
  value: AdminKind[]
  onChange: (next: AdminKind[]) => void
  disabled?: boolean
}) {
  const { t } = useTranslation('admin')
  return (
    <fieldset className="flex flex-col gap-2" disabled={disabled}>
      <legend className="mb-1 text-sm font-medium text-text">
        {t('notificationSettings.events')}
      </legend>
      {ADMIN_KINDS.map((kind) => (
        <CheckRow
          key={kind}
          label={t(`notificationSettings.kinds.${kind}`)}
          checked={value.includes(kind)}
          disabled={disabled}
          onCheckedChange={() => onChange(toggleKind(value, kind, ADMIN_KINDS))}
        />
      ))}
    </fieldset>
  )
}

function CheckRow({ label, ...props }: { label: string } & ComponentProps<typeof Checkbox>) {
  const id = useId()
  return (
    <div className="flex items-center gap-2.5">
      <Checkbox id={id} {...props} />
      <Label htmlFor={id} className="cursor-pointer font-normal text-text-2">
        {label}
      </Label>
    </div>
  )
}

function emailDefaults(channel?: Channel): EmailForm {
  const s = channel?.email
  return {
    host: s?.host ?? '',
    port: String(s?.port ?? DEFAULT_PORT.starttls),
    security: s?.security ?? 'starttls',
    username: s?.username ?? '',
    password: '',
    fromAddress: s?.fromAddress ?? '',
    fromName: s?.fromName ?? 'Starr',
    adminAddress: s?.adminAddress ?? '',
  }
}

/** The one SMTP server: member emails, and admin notifications to one address. */
function EmailCard({ channel, onDirty }: { channel?: Channel; onDirty: DirtyReport }) {
  const { t } = useTranslation(['admin', 'common', 'auth'])
  const create = useCreateChannel()
  const update = useUpdateChannel()
  // A new server or password needs a fresh re-authentication (the server asks for it).
  const elevation = useElevation()
  const remove = useDeleteChannel()
  const test = useChannelTest()
  const errorMessage = useApiErrorMessage()
  const savedEvents = adminKinds(channel?.adminEvents ?? [])
  const [events, setEvents] = useState<AdminKind[]>(savedEvents)
  const [enabled, setEnabled] = useState(channel?.enabled ?? true)
  const [removing, setRemoving] = useState(false)
  const form = useForm<EmailForm, unknown, z.output<typeof emailSchema>>({
    resolver: zodResolver(emailSchema),
    defaultValues: emailDefaults(channel),
  })
  const {
    register,
    formState: { errors },
  } = form
  const dirty =
    form.formState.isDirty ||
    enabled !== (channel?.enabled ?? true) ||
    events.join() !== savedEvents.join()
  useEffect(() => onDirty('email', dirty), [dirty, onDirty])
  useEffect(() => () => onDirty('email', false), [onDirty])

  const onSubmit = form.handleSubmit(async (v) => {
    const email = {
      host: v.host,
      port: v.port,
      security: v.security,
      username: v.username || null,
      fromAddress: v.fromAddress,
      fromName: v.fromName || null,
      adminAddress: v.adminAddress || null,
    }
    try {
      if (channel) {
        await elevation.run(() =>
          update.mutateAsync({
            id: channel.id,
            body: {
              email,
              enabled,
              adminEvents: events,
              secret: v.password === '' ? undefined : v.password,
            },
          }),
        )
      } else {
        await elevation.run(() =>
          create.mutateAsync({
            kind: 'email',
            name: t('notificationSettings.email.name'),
            email,
            enabled,
            adminEvents: events,
            secret: v.password === '' ? undefined : v.password,
          }),
        )
      }
      form.reset({ ...v, port: String(v.port), password: '' })
      toast(t('notificationSettings.saved'))
    } catch (error) {
      // Re-authentication closed: nothing saved, the form keeps its values.
      if (error instanceof ElevationCancelled) return
      toast.error(errorMessage(error))
    }
  })

  return (
    <Card className="gap-0 p-0">
      <form onSubmit={onSubmit} noValidate className="flex flex-col">
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-line px-5 py-4">
          {channel ? (
            <ChannelStatus channel={channel} />
          ) : (
            <p className="text-sm text-text-2">{t('notificationSettings.email.none')}</p>
          )}
          <SendSwitch
            label={t('notificationSettings.email.enabled')}
            checked={enabled}
            onCheckedChange={setEnabled}
          />
        </div>
        <div className="grid gap-4 px-5 py-5 sm:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
          <FormField
            label={t('notificationSettings.email.host')}
            placeholder="smtp.example.org"
            error={errors.host?.message}
            autoComplete="off"
            {...register('host')}
          />
          <FormField
            label={t('notificationSettings.email.port')}
            inputMode="numeric"
            error={errors.port?.message}
            {...register('port')}
          />
          <SelectField
            label={t('notificationSettings.email.security')}
            {...register('security', {
              onChange: (e: { target: { value: SmtpSecurity } }) =>
                form.setValue('port', String(DEFAULT_PORT[e.target.value])),
            })}
          >
            {SECURITY.map((s) => (
              <option key={s} value={s}>
                {t(`notificationSettings.email.securityOptions.${s}`)}
              </option>
            ))}
          </SelectField>
          <div className="hidden sm:block" />
          <FormField
            label={t('notificationSettings.email.username')}
            hint={t('notificationSettings.email.usernameHint')}
            autoComplete="off"
            {...register('username')}
          />
          <FormField
            label={t('notificationSettings.email.password')}
            type="password"
            autoComplete="new-password"
            placeholder={channel?.hasSecret ? t('notificationSettings.email.passwordKept') : ''}
            {...register('password')}
          />
          <FormField
            label={t('notificationSettings.email.fromAddress')}
            placeholder="starr@example.org"
            error={errors.fromAddress?.message}
            {...register('fromAddress')}
          />
          <FormField label={t('notificationSettings.email.fromName')} {...register('fromName')} />
          <div className="sm:col-span-2">
            <FormField
              label={t('notificationSettings.email.adminAddress')}
              hint={t('notificationSettings.email.adminAddressHint')}
              error={errors.adminAddress?.message}
              {...register('adminAddress')}
            />
          </div>
          <div className="sm:col-span-2">
            <EventChoice value={events} onChange={setEvents} />
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2 border-t border-line px-5 py-4">
          <Button
            type="submit"
            variant="primary"
            // A stored channel saves only changes (F8); a new one shows what is missing.
            disabled={create.isPending || update.isPending || (channel !== undefined && !dirty)}
          >
            {t('common:actions.save')}
          </Button>
          {channel && (
            <>
              <Button
                type="button"
                disabled={test.pending || dirty}
                title={dirty ? t('notificationSettings.test.saveFirst') : undefined}
                onClick={() => test.start(channel.id)}
              >
                <SendIcon aria-hidden />
                {test.pending
                  ? t('notificationSettings.test.running')
                  : t('notificationSettings.test.send')}
              </Button>
              <Button
                type="button"
                variant="ghost"
                className="ml-auto"
                disabled={remove.isPending}
                onClick={() => setRemoving(true)}
              >
                <Trash2Icon aria-hidden />
                {t('notificationSettings.remove')}
              </Button>
              <ConfirmDialog
                open={removing}
                onOpenChange={setRemoving}
                title={t('notificationSettings.email.removeTitle')}
                description={t('notificationSettings.email.removeText')}
                confirmLabel={t('notificationSettings.remove')}
                pending={remove.isPending}
                onConfirm={() =>
                  // Promise callbacks: the card remounts once the list no longer has it.
                  remove.mutateAsync(channel.id).then(
                    () => toast(t('notificationSettings.email.removed')),
                    (error: unknown) => toast.error(errorMessage(error)),
                  )
                }
              />
            </>
          )}
        </div>
      </form>
      {elevation.dialog}
    </Card>
  )
}

type DirtyReport = (key: string, dirty: boolean) => void

/** The one on/off control of a channel, named by what it does (ADM-26). */
function SendSwitch({
  label,
  checked,
  disabled,
  onCheckedChange,
}: {
  label: string
  checked: boolean
  disabled?: boolean
  onCheckedChange: (checked: boolean) => void
}) {
  const id = useId()
  return (
    <div className="flex items-center gap-2.5">
      <Switch id={id} checked={checked} disabled={disabled} onCheckedChange={onCheckedChange} />
      <Label htmlFor={id} className="cursor-pointer font-normal text-text-2">
        {label}
      </Label>
    </div>
  )
}

function SelectField({
  label,
  children,
  ...props
}: { label: string; children: ReactNode } & ComponentProps<'select'>) {
  const id = useId()
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <select
        id={id}
        className={cn(
          'h-10 w-full cursor-pointer rounded-control border border-line bg-night-0 px-3 text-base text-text transition-colors outline-hidden',
          'hover:border-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-0 focus-visible:outline-polaris/40',
        )}
        {...props}
      >
        {children}
      </select>
    </div>
  )
}

/** A webhook saves like the email card (ADM-26): change the switch and events, then Save. */
function WebhookRow({ channel, onDirty }: { channel: Channel; onDirty: DirtyReport }) {
  const { t } = useTranslation(['admin', 'common'])
  const update = useUpdateChannel()
  const remove = useDeleteChannel()
  const test = useChannelTest()
  const errorMessage = useApiErrorMessage()
  const onError = (error: unknown) => toast.error(errorMessage(error))
  const savedEvents = adminKinds(channel.adminEvents)
  const [enabled, setEnabled] = useState(channel.enabled)
  const [events, setEvents] = useState<AdminKind[]>(savedEvents)
  const [removing, setRemoving] = useState(false)
  const dirty = enabled !== channel.enabled || events.join() !== savedEvents.join()
  const key = `hook:${channel.id}`
  useEffect(() => onDirty(key, dirty), [key, dirty, onDirty])
  useEffect(() => () => onDirty(key, false), [key, onDirty])

  return (
    <li className="flex flex-col gap-4 px-5 py-4">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="flex items-center gap-2">
            <span className="truncate font-medium text-text">{channel.name}</span>
            <span className="type-data rounded-full border border-line px-2 text-xs text-text-2">
              {t(`notificationSettings.hooks.kind.${channel.kind as 'discord' | 'webhook'}`)}
            </span>
          </span>
          {channel.host && <span className="type-data text-sm text-text-3">{channel.host}</span>}
        </div>
        <SendSwitch
          label={t('notificationSettings.hooks.enabled', { name: channel.name })}
          checked={enabled}
          disabled={update.isPending}
          onCheckedChange={setEnabled}
        />
      </div>
      <ChannelStatus channel={channel} />
      <EventChoice value={events} disabled={update.isPending} onChange={setEvents} />
      <div className="flex flex-wrap gap-2">
        <Button
          size="sm"
          disabled={!dirty || update.isPending}
          onClick={() =>
            update
              .mutateAsync({ id: channel.id, body: { enabled, adminEvents: events } })
              .then(() => toast(t('notificationSettings.saved')), onError)
          }
        >
          {t('common:actions.save')}
        </Button>
        <Button
          size="sm"
          disabled={test.pending || !channel.enabled || dirty}
          title={dirty ? t('notificationSettings.test.saveFirst') : undefined}
          onClick={() => test.start(channel.id)}
        >
          <SendIcon aria-hidden />
          {test.pending
            ? t('notificationSettings.test.running')
            : t('notificationSettings.test.send')}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="ml-auto"
          disabled={remove.isPending}
          onClick={() => setRemoving(true)}
        >
          <Trash2Icon aria-hidden />
          {t('notificationSettings.remove')}
        </Button>
      </div>
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t('notificationSettings.hooks.removeTitle', { name: channel.name })}
        description={t('notificationSettings.hooks.removeText')}
        confirmLabel={t('notificationSettings.remove')}
        pending={remove.isPending}
        onConfirm={() =>
          // Promise callbacks: the row unmounts once the list no longer has it.
          remove
            .mutateAsync(channel.id)
            .then(
              () => toast(t('notificationSettings.hooks.removed', { name: channel.name })),
              onError,
            )
        }
      />
    </li>
  )
}

function AddWebhook() {
  const { t } = useTranslation(['admin', 'common'])
  const [open, setOpen] = useState(false)
  // A new channel is a new place messages go: re-authenticate before the dialog (ADM-34).
  const elevated = useEnsureElevated()
  return (
    <>
      {elevated.dialog}
      <Button size="sm" onClick={() => elevated.ensure(() => setOpen(true))}>
        <PlusIcon aria-hidden />
        {t('notificationSettings.hooks.add')}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-w-lg">
          {open && <WebhookFormBody onClose={() => setOpen(false)} />}
        </DialogContent>
      </Dialog>
    </>
  )
}

function WebhookFormBody({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation(['admin', 'common'])
  const create = useCreateChannel()
  const errorMessage = useApiErrorMessage()
  const [events, setEvents] = useState<AdminKind[]>(ADMIN_KINDS)
  const [failure, setFailure] = useState<unknown>(null)
  const {
    register,
    handleSubmit,
    formState: { errors },
  } = useForm<WebhookForm>({
    resolver: zodResolver(webhookSchema),
    defaultValues: { kind: 'discord', name: '', url: '' },
  })
  const onSubmit = handleSubmit((v) =>
    create.mutate(
      { kind: v.kind, name: v.name, secret: v.url, adminEvents: events },
      {
        onSuccess: () => {
          toast(t('notificationSettings.saved'))
          onClose()
        },
        onError: setFailure,
      },
    ),
  )
  return (
    <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
      <DialogHeader>
        <DialogTitle>{t('notificationSettings.hooks.addTitle')}</DialogTitle>
        <DialogDescription>{t('notificationSettings.hooks.addDescription')}</DialogDescription>
      </DialogHeader>
      <DialogBody>
        {failure != null && <ErrorNotice>{errorMessage(failure)}</ErrorNotice>}
        <SelectField label={t('notificationSettings.hooks.type')} {...register('kind')}>
          <option value="discord">{t('notificationSettings.hooks.kind.discord')}</option>
          <option value="webhook">{t('notificationSettings.hooks.kind.webhook')}</option>
        </SelectField>
        <FormField
          label={t('notificationSettings.hooks.name')}
          placeholder={t('notificationSettings.hooks.namePlaceholder')}
          error={errors.name?.message}
          {...register('name')}
        />
        <FormField
          label={t('notificationSettings.hooks.url')}
          hint={t('notificationSettings.hooks.urlHint')}
          type="password"
          autoComplete="off"
          error={errors.url?.message}
          {...register('url')}
        />
        <EventChoice value={events} onChange={setEvents} />
      </DialogBody>
      <DialogFooter>
        <Button type="button" variant="ghost" onClick={onClose}>
          {t('common:actions.cancel')}
        </Button>
        <Button type="submit" variant="primary" disabled={create.isPending}>
          {t('notificationSettings.hooks.save')}
        </Button>
      </DialogFooter>
    </form>
  )
}
