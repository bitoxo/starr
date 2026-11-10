import { zodResolver } from '@hookform/resolvers/zod'
import {
  CheckIcon,
  CopyIcon,
  DownloadIcon,
  KeyRoundIcon,
  MonitorSmartphoneIcon,
  ShieldCheckIcon,
  ShieldOffIcon,
} from 'lucide-react'
import { useId, useState, type ReactNode } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import { useMembers } from '@/api/members'
import {
  useChangeOwnPassword,
  useConfirmTotp,
  useDisableTotp,
  useEnrollTotp,
  useOwnSessions,
  useRegenerateRecoveryCodes,
  useRevokeOwnSession,
  useSetLinkedMember,
  useTotpStatus,
  type TotpEnrollment,
} from '@/api/security'
import { useSession } from '@/auth/session'
import { PageHeader } from '@/components/AppShell'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import { FormField } from '@/components/FormField'
import { ErrorNotice } from '@/components/members'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { Card, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { deviceLabel, formatRelative } from '@/lib/format'
import {
  changePasswordSchema,
  password,
  PASSWORD_MIN,
  secondFactor,
  totpCode,
} from '@/lib/validation'

export default function SecurityPage() {
  const { t } = useTranslation('admin')
  const me = useSession().me
  const required = me?.totpEnrollmentRequired ?? false

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('security.title')}
        description={t('security.description')}
      />
      {/* While enrollment is required, nothing else is reachable. */}
      <div className="flex max-w-xl flex-col gap-6">
        {required && (
          <div
            role="status"
            className="chip rounded-card px-4 py-3 text-base [--chip:var(--color-warn)]"
          >
            {t('security.required')}
          </div>
        )}
        <TotpCard />
        {!required && (
          <>
            <PasswordCard />
            <SessionsCard />
            <LinkedMemberCard />
          </>
        )}
      </div>
    </>
  )
}

/** The admin's own password (ADM-28): the current one, then the new one twice. */
export function PasswordCard() {
  const { t } = useTranslation(['admin', 'auth'])
  const change = useChangeOwnPassword()
  const errorMessage = useApiErrorMessage()
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors, isSubmitting },
  } = useForm<z.infer<typeof changePasswordSchema>>({
    resolver: zodResolver(changePasswordSchema),
    defaultValues: { currentPassword: '', password: '', confirm: '' },
  })
  const onSubmit = handleSubmit(async ({ currentPassword, password }) => {
    const done = await change
      .mutateAsync({ currentPassword, newPassword: password })
      .then(() => true)
      .catch(() => false)
    if (done) {
      reset()
      toast(t('security.passwordChanged'))
    }
  })
  return (
    <SecurityCard title={t('security.passwordTitle')}>
      <form onSubmit={onSubmit} noValidate className="flex flex-col gap-4 sm:max-w-sm">
        <p className="text-text-2">{t('security.passwordText')}</p>
        {change.isError && <ErrorNotice>{errorMessage(change.error)}</ErrorNotice>}
        <FormField
          label={t('auth:fields.currentPassword')}
          error={errors.currentPassword?.message}
          type="password"
          autoComplete="current-password"
          {...register('currentPassword')}
        />
        <FormField
          label={t('auth:fields.newPassword')}
          hint={t('auth:fields.passwordHint', { count: PASSWORD_MIN })}
          error={errors.password?.message}
          type="password"
          autoComplete="new-password"
          {...register('password')}
        />
        <FormField
          label={t('auth:fields.confirmPassword')}
          error={errors.confirm?.message}
          type="password"
          autoComplete="new-password"
          {...register('confirm')}
        />
        <Button type="submit" className="self-start" disabled={isSubmitting}>
          {t('security.changePassword')}
        </Button>
      </form>
    </SecurityCard>
  )
}

/** Where the admin is signed in (ADM-28); other sessions can be ended one by one. */
export function SessionsCard() {
  const { t } = useTranslation('admin')
  const sessions = useOwnSessions()
  const revoke = useRevokeOwnSession()
  const errorMessage = useApiErrorMessage()
  return (
    <SecurityCard title={t('security.sessionsTitle')}>
      {sessions.isPending && <Skeleton className="h-16 w-full" />}
      {sessions.isError && (
        <QueryError
          message={t('security.sessionsFailed')}
          onRetry={() => void sessions.refetch()}
        />
      )}
      {sessions.data && (
        <ul className="flex flex-col divide-y divide-line rounded-card border border-line">
          {sessions.data.map((s) => {
            const device = deviceLabel(s.userAgent) ?? t('security.unknownDevice')
            return (
              <li key={s.id} className="flex items-center gap-3 px-4 py-3">
                <MonitorSmartphoneIcon aria-hidden className="size-5 shrink-0 text-text-3" />
                <div className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate text-base text-text">{device}</span>
                  <span className="type-data text-sm text-text-3">
                    {s.current
                      ? t('security.thisDevice')
                      : t('security.lastSeen', { when: formatRelative(s.lastSeenAt) })}
                    {s.ip ? ` · ${s.ip}` : ''}
                  </span>
                </div>
                {!s.current && (
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-label={t('security.signOutDevice', { device })}
                    disabled={revoke.isPending}
                    onClick={() =>
                      revoke.mutate(s.id, {
                        onSuccess: () => toast(t('security.signedOut', { device })),
                        onError: (error) => toast.error(errorMessage(error)),
                      })
                    }
                  >
                    {t('security.signOut')}
                  </Button>
                )}
              </li>
            )
          })}
        </ul>
      )}
    </SecurityCard>
  )
}

function SecurityCard({
  title,
  status,
  children,
}: {
  title: string
  status?: ReactNode
  children: ReactNode
}) {
  return (
    <Card className="gap-0 p-0">
      <CardHeader className="flex flex-row items-center justify-between gap-3 px-5 py-4">
        <CardTitle>{title}</CardTitle>
        {status}
      </CardHeader>
      <Separator />
      <div className="flex flex-col gap-4 px-5 py-4 text-base">{children}</div>
    </Card>
  )
}

function TotpCard() {
  const { t } = useTranslation('admin')
  const enabled = useSession().me?.totpEnabled ?? false
  const status = useTotpStatus()
  const enroll = useEnrollTotp()
  const errorMessage = useApiErrorMessage()
  const [codes, setCodes] = useState<string[] | null>(null)
  const [disabling, setDisabling] = useState(false)
  const [regenerating, setRegenerating] = useState(false)

  const chip = enabled ? (
    <StatusChip tone="ok">{t('security.totpOn')}</StatusChip>
  ) : (
    <StatusChip tone="neutral">{t('security.totpOff')}</StatusChip>
  )

  return (
    <SecurityCard title={t('security.totpTitle')} status={chip}>
      {codes ? (
        <RecoveryCodesPanel
          codes={codes}
          onDone={() => {
            setCodes(null)
            enroll.reset()
          }}
        />
      ) : enabled ? (
        <>
          <p className="text-text-2">{t('security.totpOnText')}</p>
          {status.data && (
            <p className="type-data text-sm text-text-3">
              {t('security.recoveryLeft', { count: status.data.recoveryCodesRemaining })}
            </p>
          )}
          <div className="flex flex-wrap gap-2">
            <Button onClick={() => setRegenerating(true)}>
              <KeyRoundIcon aria-hidden />
              {t('security.regenerate')}
            </Button>
            <Button variant="ghost" onClick={() => setDisabling(true)}>
              <ShieldOffIcon aria-hidden />
              {t('security.disable')}
            </Button>
          </div>
        </>
      ) : enroll.data ? (
        <EnrollmentPanel
          enrollment={enroll.data}
          onConfirmed={(recovery) => {
            setCodes(recovery)
            toast(t('security.enabledToast'))
          }}
        />
      ) : (
        <>
          <p className="text-text-2">{t('security.totpOffText')}</p>
          {enroll.isError && <ErrorNotice>{errorMessage(enroll.error)}</ErrorNotice>}
          <Button
            variant="primary"
            className="self-start"
            disabled={enroll.isPending}
            onClick={() => enroll.mutate()}
          >
            <ShieldCheckIcon aria-hidden />
            {t('security.enable')}
          </Button>
        </>
      )}
      <DisableTotpDialog open={disabling} onOpenChange={setDisabling} />
      <RegenerateCodesDialog
        open={regenerating}
        onOpenChange={setRegenerating}
        onRegenerated={setCodes}
      />
    </SecurityCard>
  )
}

function EnrollmentPanel({
  enrollment,
  onConfirmed,
}: {
  enrollment: TotpEnrollment
  onConfirmed: (codes: string[]) => void
}) {
  const { t } = useTranslation('admin')
  return (
    <>
      <p className="text-text-2">{t('security.scan')}</p>
      <div className="flex flex-wrap items-start gap-5">
        {/* Dark modules on white with a quiet zone: what scanners need. */}
        <img
          src={`data:image/svg+xml;base64,${enrollment.qrSvg}`}
          alt={t('security.qrAlt')}
          width={180}
          height={180}
          className="rounded-card bg-white"
        />
        <div className="flex min-w-0 flex-1 flex-col gap-1.5">
          <span className="type-data text-xs tracking-wide text-text-3 uppercase">
            {t('security.manualKey')}
          </span>
          <code className="type-data text-sm break-all text-text select-all">
            {enrollment.secret.replace(/(.{4})/g, '$1 ').trim()}
          </code>
        </div>
      </div>
      <ConfirmTotpForm onConfirmed={onConfirmed} />
    </>
  )
}

const confirmSchema = z.object({ code: secondFactor })

/** Step 2 of enrollment: the first code from the app turns TOTP on. */
export function ConfirmTotpForm({ onConfirmed }: { onConfirmed: (codes: string[]) => void }) {
  const { t } = useTranslation('admin')
  const confirm = useConfirmTotp()
  const errorMessage = useApiErrorMessage()
  const {
    register,
    handleSubmit,
    formState: { errors },
  } = useForm<z.infer<typeof confirmSchema>>({
    resolver: zodResolver(confirmSchema),
    defaultValues: { code: '' },
  })
  const onSubmit = handleSubmit(async ({ code }) => {
    const result = await confirm.mutateAsync(code).catch(() => null)
    if (result) onConfirmed(result.codes)
  })
  return (
    <form onSubmit={onSubmit} noValidate className="flex flex-col gap-4">
      {confirm.isError && <ErrorNotice>{errorMessage(confirm.error)}</ErrorNotice>}
      <FormField
        label={t('security.confirmCode')}
        error={errors.code?.message}
        autoComplete="one-time-code"
        inputMode="numeric"
        className="type-data max-w-48 tracking-widest"
        {...register('code')}
      />
      <Button type="submit" variant="primary" className="self-start" disabled={confirm.isPending}>
        {t('security.confirm')}
      </Button>
    </form>
  )
}

/** Recovery codes, shown once: copy or download as text. */
export function RecoveryCodesPanel({ codes, onDone }: { codes: string[]; onDone: () => void }) {
  const { t } = useTranslation(['admin', 'common'])
  const [copied, setCopied] = useState(false)
  const text = `${codes.join('\n')}\n`
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2000)
    } catch {
      // Clipboard blocked (http, permissions): the list stays selectable.
    }
  }
  const download = () => {
    const url = URL.createObjectURL(new Blob([text], { type: 'text/plain' }))
    const link = document.createElement('a')
    link.href = url
    link.download = t('security.codesFile')
    link.click()
    URL.revokeObjectURL(url)
  }
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <h3 className="font-semibold text-text">{t('security.codesTitle')}</h3>
        <p className="text-text-2">{t('security.codesText')}</p>
      </div>
      <ol
        aria-label={t('security.codesTitle')}
        className="type-data grid grid-cols-2 gap-x-6 gap-y-1.5 rounded-card border border-line bg-night-0 px-4 py-3 text-sm text-text select-all"
      >
        {codes.map((code) => (
          <li key={code}>{code}</li>
        ))}
      </ol>
      <div className="flex flex-wrap gap-2">
        <Button type="button" onClick={() => void copy()}>
          {copied ? <CheckIcon aria-hidden /> : <CopyIcon aria-hidden />}
          {copied ? t('common:actions.copied') : t('common:actions.copy')}
        </Button>
        <Button type="button" onClick={download}>
          <DownloadIcon aria-hidden />
          {t('common:actions.download')}
        </Button>
        <Button type="button" variant="primary" onClick={onDone} className="ml-auto">
          {t('security.codesSaved')}
        </Button>
      </div>
    </div>
  )
}

const disableSchema = z.object({ password, code: secondFactor })

/** Turning TOTP off needs the password and a current (or recovery) code. */
export function DisableTotpDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useTranslation(['admin', 'auth', 'common'])
  const disable = useDisableTotp()
  const errorMessage = useApiErrorMessage()
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors },
  } = useForm<z.infer<typeof disableSchema>>({
    resolver: zodResolver(disableSchema),
    defaultValues: { password: '', code: '' },
  })
  const close = (next: boolean) => {
    onOpenChange(next)
    if (!next) {
      reset()
      disable.reset()
    }
  }
  const onSubmit = handleSubmit(async (values) => {
    const done = await disable
      .mutateAsync(values)
      .then(() => true)
      .catch(() => false)
    if (done) {
      toast(t('security.disabledToast'))
      close(false)
    }
  })
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
          <DialogHeader>
            <DialogTitle>{t('security.disableTitle')}</DialogTitle>
            <DialogDescription>{t('security.disableText')}</DialogDescription>
          </DialogHeader>
          <DialogBody>
            {disable.isError && <ErrorNotice>{errorMessage(disable.error)}</ErrorNotice>}
            <FormField
              label={t('auth:fields.password')}
              error={errors.password?.message}
              type="password"
              autoComplete="current-password"
              {...register('password')}
            />
            <FormField
              label={t('auth:fields.code')}
              error={errors.code?.message}
              autoComplete="one-time-code"
              className="type-data tracking-widest"
              {...register('code')}
            />
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => close(false)}>
              {t('common:actions.cancel')}
            </Button>
            <Button type="submit" className="border-bad text-bad" disabled={disable.isPending}>
              {t('security.disable')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

const regenerateSchema = z.object({ password, code: totpCode })

/** New recovery codes: password plus a current TOTP code (not a recovery code). */
export function RegenerateCodesDialog({
  open,
  onOpenChange,
  onRegenerated,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onRegenerated: (codes: string[]) => void
}) {
  const { t } = useTranslation(['admin', 'auth', 'common'])
  const regenerate = useRegenerateRecoveryCodes()
  const errorMessage = useApiErrorMessage()
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors },
  } = useForm<z.infer<typeof regenerateSchema>>({
    resolver: zodResolver(regenerateSchema),
    defaultValues: { password: '', code: '' },
  })
  const close = (next: boolean) => {
    onOpenChange(next)
    if (!next) {
      reset()
      regenerate.reset()
    }
  }
  const onSubmit = handleSubmit(async (values) => {
    const result = await regenerate.mutateAsync(values).catch(() => null)
    if (result) {
      onRegenerated(result.codes)
      close(false)
    }
  })
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
          <DialogHeader>
            <DialogTitle>{t('security.regenerateTitle')}</DialogTitle>
            <DialogDescription>{t('security.regenerateText')}</DialogDescription>
          </DialogHeader>
          <DialogBody>
            {regenerate.isError && <ErrorNotice>{errorMessage(regenerate.error)}</ErrorNotice>}
            <FormField
              label={t('auth:fields.password')}
              error={errors.password?.message}
              type="password"
              autoComplete="current-password"
              {...register('password')}
            />
            <FormField
              label={t('security.totpCode')}
              error={errors.code?.message}
              autoComplete="one-time-code"
              inputMode="numeric"
              className="type-data max-w-48 tracking-widest"
              {...register('code')}
            />
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => close(false)}>
              {t('common:actions.cancel')}
            </Button>
            <Button type="submit" variant="primary" disabled={regenerate.isPending}>
              {t('security.regenerateSubmit')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function LinkedMemberCard() {
  const { t } = useTranslation('admin')
  const linked = useSession().me?.linkedMember ?? null
  const setLinked = useSetLinkedMember()
  const members = useMembers({ status: 'active' })
  const errorMessage = useApiErrorMessage()
  const selectId = useId()
  const passwordId = useId()
  const [choice, setChoice] = useState('')
  const [memberPassword, setMemberPassword] = useState('')
  const [unlinking, setUnlinking] = useState(false)

  return (
    <SecurityCard title={t('security.memberTitle')}>
      <p className="text-text-2">{t('security.memberText')}</p>
      {setLinked.isError && <ErrorNotice>{errorMessage(setLinked.error)}</ErrorNotice>}
      {linked ? (
        <div className="flex flex-wrap items-center gap-3">
          <span className="flex flex-col leading-tight">
            <span className="font-medium text-text">{linked.displayName}</span>
            <span className="type-data text-sm text-text-3">{linked.username}</span>
          </span>
          {!linked.active && <StatusChip tone="warn">{t('security.memberSuspended')}</StatusChip>}
          <Button
            className="ml-auto"
            disabled={setLinked.isPending}
            onClick={() => setUnlinking(true)}
          >
            {t('security.unlink')}
          </Button>
          {/* ADM-07: relinking needs the member's password, so unlinking asks first. */}
          <ConfirmDialog
            open={unlinking}
            onOpenChange={setUnlinking}
            title={t('security.unlinkTitle', { name: linked.displayName })}
            description={t('security.unlinkText')}
            confirmLabel={t('security.unlink')}
            pending={setLinked.isPending}
            onConfirm={() =>
              setLinked.mutate(
                { memberId: null },
                {
                  onSuccess: () => {
                    setUnlinking(false)
                    toast(t('security.unlinked'))
                  },
                },
              )
            }
          />
        </div>
      ) : (
        <form
          className="flex flex-wrap items-end gap-3"
          onSubmit={(e) => {
            e.preventDefault()
            if (choice && memberPassword) {
              setLinked.mutate(
                { memberId: Number(choice), memberPassword },
                {
                  onSuccess: () => {
                    setChoice('')
                    setMemberPassword('')
                  },
                },
              )
            }
          }}
        >
          <div className="flex min-w-56 flex-1 flex-col gap-1.5">
            <Label htmlFor={selectId}>{t('security.memberPick')}</Label>
            <select
              id={selectId}
              value={choice}
              onChange={(e) => setChoice(e.target.value)}
              className="h-10 rounded-control border border-line bg-night-0 px-3 text-base text-text"
            >
              <option value="">{t('security.memberPickPlaceholder')}</option>
              {members.data?.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.displayName} ({m.username})
                </option>
              ))}
            </select>
          </div>
          <div className="flex min-w-48 flex-1 flex-col gap-1.5">
            <Label htmlFor={passwordId}>{t('security.memberPassword')}</Label>
            <Input
              id={passwordId}
              type="password"
              autoComplete="off"
              value={memberPassword}
              onChange={(e) => setMemberPassword(e.target.value)}
            />
          </div>
          <Button type="submit" disabled={!choice || !memberPassword || setLinked.isPending}>
            {t('security.link')}
          </Button>
        </form>
      )}
    </SecurityCard>
  )
}
