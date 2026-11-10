import { zodResolver } from '@hookform/resolvers/zod'
import { CheckIcon, CopyIcon, ExternalLinkIcon, KeyRoundIcon, ServerIcon } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useForm } from 'react-hook-form'
import { type TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { z } from 'zod'

import { errorCode, useApiErrorMessage } from '@/api/errors'
import {
  type MyService,
  type MyServiceState,
  useActivateServices,
  useMyServices,
  useUseStarrPassword,
} from '@/api/me'
import { PageHeader } from '@/components/AppShell'
import { EmptyState } from '@/components/EmptyState'
import { FormField } from '@/components/FormField'
import { ErrorNotice } from '@/components/members'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { StatusChip, type StatusTone } from '@/components/StatusChip'
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
import { safeHttpUrl, servicesIntro } from '@/lib/services'
import { password } from '@/lib/validation'

const TONE: Record<MyServiceState, StatusTone> = {
  ready: 'ok',
  activate: 'warn',
  oldPassword: 'info',
  settingUp: 'info',
  waiting: 'warn',
  undecided: 'neutral',
  suspended: 'bad',
}

const schema = z.object({ password })

/** What the password dialog is for. */
type Asking = { kind: 'activate' } | { kind: 'switch'; service: MyService }

/**
 * My services (spec 0005 M9): the member's media servers in member words,
 * with the server address for apps. "Activate" and "use my Starr password
 * here" ask for the current Starr password (M10 3); the password goes to
 * Starr's worker sealed and is written into the accounts once (ADR 0010).
 * One intro fits every card; each card says what it waits for, undecided
 * services included (spec 0008 slice 6, MEM-22).
 */
export default function MyServicesPage() {
  const { t } = useTranslation('member')
  const services = useMyServices()
  const errorMessage = useApiErrorMessage()
  const [asking, setAsking] = useState<Asking | null>(null)
  const list = services.data?.services ?? []
  const needsActivation = list.some((s) => s.state === 'activate')
  const intro = services.data ? servicesIntro(list.map((s) => s.state)) : null
  // The dialog gives focus back to the button that opened it (F3).
  const ask = setAsking

  return (
    <>
      <PageHeader
        variant="member"
        title={t('services.title')}
        description={t('services.description')}
        actions={
          needsActivation ? (
            <Button variant="primary" onClick={() => ask({ kind: 'activate' })}>
              <KeyRoundIcon aria-hidden />
              {t('services.activate')}
            </Button>
          ) : undefined
        }
      />
      {intro && intro !== 'empty' && (
        <p className="-mt-6 mb-8 max-w-prose text-base text-text-2">
          {t(`services.intro.${intro}`)}
        </p>
      )}
      {services.isError && (
        <QueryError
          message={errorMessage(services.error)}
          onRetry={() => void services.refetch()}
        />
      )}
      {services.isPending && (
        <ul aria-busy className="grid max-w-4xl gap-4" aria-label={t('services.loading')}>
          {[0, 1].map((i) => (
            <li
              key={i}
              className="flex flex-col gap-3 rounded-card border border-line bg-night-1 p-5"
            >
              <Skeleton className="h-6 w-40" />
              <Skeleton className="h-4 w-72 max-w-full" />
              <Skeleton className="h-4 w-56 max-w-full" />
            </li>
          ))}
        </ul>
      )}
      {services.data && list.length === 0 && (
        <EmptyState
          icon={ServerIcon}
          title={t('services.emptyTitle')}
          description={t('services.emptyText')}
        />
      )}
      {list.length > 0 && (
        <ul className="grid max-w-4xl gap-4">
          {list.map((service) => (
            <ServiceCard
              key={service.serviceId}
              service={service}
              onSwitch={() => ask({ kind: 'switch', service })}
            />
          ))}
        </ul>
      )}
      <PasswordDialog asking={asking} onClose={() => setAsking(null)} />
    </>
  )
}

/**
 * The hint under a service's state. A server without an off switch
 * (Navidrome) is locked with a random password while the member is
 * suspended; the member's password unlocks it (spec 0005 D9). A waiting
 * service says what the admin has to do (MEM-22).
 */
function serviceHint(service: MyService, t: TFunction<'member'>): string {
  if (service.lockedByStarr && service.state === 'activate') {
    return t('services.hint.activateLocked')
  }
  if (service.lockedByStarr && service.state === 'suspended') {
    return t('services.hint.suspendedLocked')
  }
  if (service.state === 'oldPassword' && !service.acceptsPassword) {
    return t('services.hint.oldPasswordLater')
  }
  if (service.state === 'waiting') {
    return t(`services.waitingFor.${service.waitingFor ?? 'admin'}`)
  }
  return t(`services.hint.${service.state}`)
}

function ServiceCard({ service, onSwitch }: { service: MyService; onSwitch: () => void }) {
  const { t } = useTranslation('member')
  const hint = serviceHint(service, t)
  const href = service.serverAddress ? safeHttpUrl(service.serverAddress) : null
  return (
    <li className="flex flex-col gap-4 rounded-card border border-line bg-night-1 p-5 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex min-w-0 flex-col gap-2">
        <div className="flex flex-wrap items-center gap-3">
          <h2 className="type-display text-xl font-bold text-text">{service.name}</h2>
          <StatusChip tone={TONE[service.state]}>{t(`services.state.${service.state}`)}</StatusChip>
        </div>
        <p className="max-w-prose text-base text-text-2">{hint}</p>
        <dl className="grid grid-cols-[max-content_minmax(0,1fr)] items-center gap-x-5 gap-y-1 text-sm">
          <dt className="text-text-3">{t('services.serverAddress')}</dt>
          <dd className="flex min-w-0 flex-wrap items-center gap-x-1">
            {service.serverAddress ? (
              <>
                {href ? (
                  <a
                    href={href}
                    target="_blank"
                    rel="noreferrer"
                    className="type-data inline-flex min-h-6 items-center gap-1.5 break-all text-text underline-offset-4 hover:underline max-lg:min-h-11"
                  >
                    {service.serverAddress}
                    <ExternalLinkIcon aria-hidden className="size-3.5 shrink-0 text-text-3" />
                    <span className="sr-only">{t('services.opensInNewTab')}</span>
                  </a>
                ) : (
                  <span className="type-data break-all text-text">{service.serverAddress}</span>
                )}
                <CopyButton
                  value={service.serverAddress}
                  label={t('services.copyAddress', { name: service.name })}
                />
              </>
            ) : (
              <span className="text-text-3">{t('services.noAddress')}</span>
            )}
          </dd>
          {service.username && (
            <>
              <dt className="text-text-3">{t('services.usernameThere')}</dt>
              <dd className="flex min-w-0 flex-wrap items-center gap-x-1">
                <span className="type-data text-text">{service.username}</span>
                <CopyButton
                  value={service.username}
                  label={t('services.copyUsername', { name: service.name })}
                />
              </dd>
            </>
          )}
        </dl>
      </div>
      {service.state === 'oldPassword' && service.acceptsPassword && (
        <Button variant="secondary" className="self-start" onClick={onSwitch}>
          {t('services.switch')}
        </Button>
      )}
    </li>
  )
}

/** Copies `value`; says "Copied." in a live region, or how to copy it by hand. */
function CopyButton({ value, label }: { value: string; label: string }) {
  const { t } = useTranslation('member')
  const [said, setSaid] = useState<'copied' | 'failed' | null>(null)
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])
  const copy = async () => {
    window.clearTimeout(timer.current)
    try {
      await navigator.clipboard.writeText(value)
      setSaid('copied')
    } catch {
      // Clipboard blocked (plain http, permissions): the text stays selectable.
      setSaid('failed')
    }
    timer.current = window.setTimeout(() => setSaid(null), 3000)
  }
  return (
    <>
      <button
        type="button"
        aria-label={label}
        onClick={() => void copy()}
        className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-full text-text-3 transition-colors hover:bg-night-2 hover:text-text max-lg:size-11"
      >
        {said === 'copied' ? (
          <CheckIcon aria-hidden className="size-4 text-ok" />
        ) : (
          <CopyIcon aria-hidden className="size-4" />
        )}
      </button>
      <span role="status" className="text-xs text-text-3">
        {said && t(said === 'copied' ? 'services.copied' : 'services.copyFailed')}
      </span>
    </>
  )
}

function PasswordDialog({ asking, onClose }: { asking: Asking | null; onClose: () => void }) {
  const { t } = useTranslation(['member', 'common'])
  const errorMessage = useApiErrorMessage()
  const activate = useActivateServices()
  const switchTo = useUseStarrPassword()
  const {
    register,
    handleSubmit,
    reset,
    setError,
    formState: { errors, isSubmitting },
  } = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: { password: '' },
  })
  const close = () => {
    if (isSubmitting) return
    reset()
    onClose()
  }
  const onSubmit = handleSubmit(async ({ password: value }) => {
    if (!asking) return
    try {
      if (asking.kind === 'activate') {
        await activate.mutateAsync(value)
        toast(t('services.activated'))
      } else {
        await switchTo.mutateAsync({ serviceId: asking.service.serviceId, password: value })
        toast(t('services.switched', { name: asking.service.name }))
      }
    } catch (error) {
      // The field is "Your Starr password": a wrong one is said there, in its words (MEM-22).
      if (errorCode(error) === 'auth.current_password_invalid') {
        setError('password', { message: 'starrPasswordWrong' }, { shouldFocus: true })
      } else {
        setError('root', { message: errorMessage(error) })
      }
      return
    }
    reset()
    onClose()
  })
  const name = asking?.kind === 'switch' ? asking.service.name : ''
  return (
    <Dialog open={asking !== null} onOpenChange={(open) => !open && close()}>
      <DialogContent>
        {/* No autoFocus: the dialog focuses the field itself and remembers the opener (F3). */}
        <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
          <DialogHeader>
            <DialogTitle>
              {asking?.kind === 'switch'
                ? t('services.switchTitle', { name })
                : t('services.activateTitle')}
            </DialogTitle>
            <DialogDescription>
              {asking?.kind === 'switch'
                ? t('services.switchText', { name })
                : t('services.activateText')}
            </DialogDescription>
          </DialogHeader>
          <DialogBody>
            {asking?.kind === 'switch' && asking.service.passwordChangeSignsOut && (
              <p className="chip rounded-card px-4 py-3 text-sm [--chip:var(--color-warn)]">
                {t('services.switchSignsOut', { name })}
              </p>
            )}
            {errors.root?.message && <ErrorNotice>{errors.root.message}</ErrorNotice>}
            <FormField
              label={t('services.password')}
              error={errors.password?.message}
              type="password"
              autoComplete="current-password"
              {...register('password')}
            />
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={close}>
              {t('common:actions.cancel')}
            </Button>
            <Button type="submit" variant="primary" disabled={isSubmitting}>
              <KeyRoundIcon aria-hidden />
              {asking?.kind === 'switch' ? t('services.switch') : t('services.activate')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
