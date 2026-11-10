import { zodResolver } from '@hookform/resolvers/zod'
import { LogOutIcon, MonitorSmartphoneIcon, ServerIcon } from 'lucide-react'
import { useId, type ReactNode } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { NavLink, Outlet, useNavigate } from 'react-router'
import { toast } from 'sonner'
import type { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import { useNotificationHistory } from '@/api/notifications'
import {
  useChangePassword,
  useLogOutEverywhere,
  useMyServices,
  useProfile,
  useRevokeSession,
  useSessions,
  useUpdateProfile,
} from '@/api/me'
import { PageHeader } from '@/components/AppShell'
import { FormField } from '@/components/FormField'
import { CategoryList, ErrorNotice } from '@/components/members'
import { NotificationHistory } from '@/components/NotificationBell'
import { setShortcutsEnabled, useShortcutsEnabled } from '@/components/shell/prefs'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { deviceLabel, formatRelative } from '@/lib/format'
import { cn } from '@/lib/utils'
import { changePasswordSchema, PASSWORD_MIN, profileSchema } from '@/lib/validation'

import { NotificationPreferences } from './NotificationPreferences'

function Panel({
  title,
  description,
  children,
}: {
  title: string
  description?: string
  children: ReactNode
}) {
  return (
    <section className="grid gap-6 border-t border-line py-10 first:border-t-0 first:pt-0 md:grid-cols-[minmax(0,1fr)_minmax(0,1.6fr)]">
      <div className="flex flex-col gap-1.5">
        <h2 className="type-display text-xl font-bold text-text">{title}</h2>
        {description && <p className="text-base text-text-2">{description}</p>}
      </div>
      <div className="flex min-w-0 flex-col gap-5">{children}</div>
    </section>
  )
}

const TABS = [
  { to: '/profile', key: 'account', end: true },
  { to: '/profile/notifications', key: 'notifications', end: false },
  { to: '/profile/sessions', key: 'sessions', end: false },
  { to: '/profile/preferences', key: 'preferences', end: false },
] as const

const tabClass = ({ isActive }: { isActive: boolean }) =>
  cn(
    'inline-flex min-h-8 items-center gap-2 rounded-full px-3.5 py-1 text-sm text-text-2 transition-colors hover:text-text max-lg:min-h-11',
    isActive && 'bg-night-2 text-text',
  )

/**
 * Profile with its sections as routes (spec 0008 IA): Account, Notifications, Sessions,
 * Preferences. Below 1024 px, where the tab bar has no room for it, My services is reached from
 * here too (F-6).
 */
export default function ProfilePage() {
  const { t } = useTranslation('member')
  return (
    <>
      <PageHeader
        variant="member"
        title={t('profile.title')}
        description={t('profile.description')}
      />
      <nav
        aria-label={t('profile.tabs.label')}
        className="mb-10 flex flex-wrap gap-1 rounded-card lg:inline-flex lg:rounded-full lg:border lg:border-line lg:bg-night-1 lg:p-0.5"
      >
        {TABS.map((tab) => (
          <NavLink key={tab.key} to={tab.to} end={tab.end} className={tabClass}>
            {t(`profile.tabs.${tab.key}`)}
          </NavLink>
        ))}
        <NavLink to="/services" className={(state) => cn(tabClass(state), 'lg:hidden')}>
          <ServerIcon aria-hidden className="size-4" strokeWidth={1.75} />
          {t('profile.tabs.services')}
        </NavLink>
      </nav>
      <div className="max-w-4xl">
        <Outlet />
      </div>
    </>
  )
}

/** `/profile`: account, what the member may request, password. */
export function ProfileAccount() {
  const { t } = useTranslation('member')
  const profile = useProfile()
  const errorMessage = useApiErrorMessage()
  if (profile.isError) return <ErrorNotice>{errorMessage(profile.error)}</ErrorNotice>
  if (!profile.data) return null
  return (
    <>
      <Panel title={t('profile.account')} description={t('profile.accountDescription')}>
        <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-base">
          <dt className="text-text-3">{t('profile.username')}</dt>
          <dd className="type-data text-text">{profile.data.username}</dd>
          {profile.data.email && (
            <>
              <dt className="text-text-3">{t('profile.email')}</dt>
              <dd className="text-text">{profile.data.email}</dd>
            </>
          )}
        </dl>
        <DisplayNameForm key={profile.data.displayName} current={profile.data.displayName} />
      </Panel>
      <Panel title={t('profile.entitlements')} description={t('profile.entitlementsDescription')}>
        <CategoryList entitlements={profile.data.entitlements} empty={t('profile.off')} />
      </Panel>
      <Panel title={t('profile.password')} description={t('profile.passwordDescription')}>
        <PasswordForm />
      </Panel>
    </>
  )
}

/** `/profile/notifications`: the whole stream, page by page (MEM-21), then the email choices. */
export function ProfileNotifications() {
  const { t } = useTranslation('member')
  // The same query as the list: the email choices come once it is there, so they never jump.
  const history = useNotificationHistory()
  return (
    <>
      <Panel
        title={t('notifications.historyTitle')}
        description={t('notifications.historyDescription')}
      >
        <NotificationHistory />
      </Panel>
      {!history.isPending && (
        <Panel title={t('notifications.emailTitle')} description={t('notifications.description')}>
          <NotificationPreferences />
        </Panel>
      )}
    </>
  )
}

/** `/profile/sessions`. */
export function ProfileSessions() {
  const { t } = useTranslation('member')
  return (
    <Panel title={t('profile.sessions')} description={t('profile.sessionsDescription')}>
      <Sessions />
    </Panel>
  )
}

/**
 * `/profile/preferences`: settings of this browser only. Single-key shortcuts can be switched
 * off for speech input and other assistive technology (WCAG 2.1.4, spec 0008 F-1).
 */
export function ProfilePreferences() {
  const { t } = useTranslation('member')
  const shortcuts = useShortcutsEnabled()
  const id = useId()
  return (
    <Panel
      title={t('profile.preferences.title')}
      description={t('profile.preferences.description')}
    >
      <div className="flex items-start gap-3">
        <Switch
          id={id}
          checked={shortcuts}
          onCheckedChange={setShortcutsEnabled}
          aria-describedby={`${id}-hint`}
          className="mt-1"
        />
        <div className="flex flex-col gap-1">
          <label htmlFor={id} className="text-base font-medium text-text">
            {t('profile.preferences.shortcuts')}
          </label>
          <p id={`${id}-hint`} className="text-sm text-text-2">
            {t('profile.preferences.shortcutsHint')}
          </p>
          <dl className="mt-2 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1.5 text-sm text-text-2">
            <dt>
              <kbd className="type-data rounded-[4px] border border-line px-1.5 text-text">[</kbd>
            </dt>
            <dd>{t('profile.preferences.keySidebar')}</dd>
            <dt>
              <kbd className="type-data rounded-[4px] border border-line px-1.5 text-text">/</kbd>
            </dt>
            <dd>{t('profile.preferences.keySearch')}</dd>
          </dl>
        </div>
      </div>
    </Panel>
  )
}

function DisplayNameForm({ current }: { current: string }) {
  const { t } = useTranslation(['member', 'auth', 'common'])
  const update = useUpdateProfile()
  const errorMessage = useApiErrorMessage()
  const {
    register,
    handleSubmit,
    formState: { errors, isDirty },
  } = useForm<z.infer<typeof profileSchema>>({
    resolver: zodResolver(profileSchema),
    defaultValues: { displayName: current },
  })
  const onSubmit = handleSubmit(({ displayName }) =>
    update.mutate(displayName, {
      onSuccess: () => toast(t('profile.saved')),
      onError: (error) => toast.error(errorMessage(error)),
    }),
  )
  return (
    <form onSubmit={onSubmit} noValidate className="flex flex-col gap-4 sm:max-w-sm">
      <FormField
        label={t('auth:fields.displayName')}
        error={errors.displayName?.message}
        autoComplete="nickname"
        {...register('displayName')}
      />
      <Button type="submit" className="self-start" disabled={!isDirty || update.isPending}>
        {t('common:actions.save')}
      </Button>
    </form>
  )
}

function PasswordForm() {
  const { t } = useTranslation(['member', 'auth'])
  const change = useChangePassword()
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
      toast(t('profile.passwordChanged'))
    }
  })
  // Story 4 (spec 0005): say which services sign the member's devices out.
  const signsOut = (useMyServices().data?.services ?? [])
    .filter((s) => s.usesStarrPassword && s.passwordChangeSignsOut)
    .map((s) => s.name)
  return (
    <form onSubmit={onSubmit} noValidate className="flex flex-col gap-4 sm:max-w-sm">
      {change.isError && <ErrorNotice>{errorMessage(change.error)}</ErrorNotice>}
      {signsOut.length > 0 && (
        <p className="chip rounded-card px-4 py-3 text-sm [--chip:var(--color-warn)]">
          {t('profile.passwordServices', { names: signsOut.join(', ') })}
        </p>
      )}
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
        {t('profile.changePassword')}
      </Button>
    </form>
  )
}

function Sessions() {
  const { t } = useTranslation('member')
  const sessions = useSessions()
  const revoke = useRevokeSession()
  const everywhere = useLogOutEverywhere()
  const errorMessage = useApiErrorMessage()
  const navigate = useNavigate()
  const onError = (error: unknown) => toast.error(errorMessage(error))

  return (
    <>
      {sessions.isError && <ErrorNotice>{errorMessage(sessions.error)}</ErrorNotice>}
      <ul className="flex flex-col divide-y divide-line rounded-card border border-line">
        {sessions.data?.map((s) => (
          <li key={s.id} className="flex items-center gap-3 px-4 py-3">
            <MonitorSmartphoneIcon aria-hidden className="size-5 shrink-0 text-text-3" />
            <div className="flex min-w-0 flex-1 flex-col">
              <span className="truncate text-base text-text">
                {deviceLabel(s.userAgent) ?? t('profile.unknownDevice')}
              </span>
              <span className="type-data text-sm text-text-3">
                {s.current
                  ? t('profile.thisDevice')
                  : t('profile.lastSeen', { when: formatRelative(s.lastSeenAt) })}
                {s.ip ? ` · ${s.ip}` : ''}
              </span>
            </div>
            {!s.current && (
              <Button
                size="sm"
                variant="ghost"
                disabled={revoke.isPending}
                onClick={() => revoke.mutate(s.id, { onError })}
              >
                {t('profile.revoke')}
              </Button>
            )}
          </li>
        ))}
      </ul>
      <div className="flex flex-col items-start gap-1.5">
        <Button
          disabled={everywhere.isPending}
          onClick={() =>
            everywhere.mutate(undefined, {
              onSuccess: () => navigate('/login', { replace: true }),
              onError,
            })
          }
        >
          <LogOutIcon aria-hidden />
          {t('profile.logOutEverywhere')}
        </Button>
        <p className="text-sm text-text-3">{t('profile.logOutEverywhereHint')}</p>
      </div>
    </>
  )
}
