import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useForm, type UseFormRegisterReturn } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Link, Navigate, useParams } from 'react-router'
import type { z } from 'zod'

import { api } from '@/api/client'
import { errorCode, unwrap, useApiErrorMessage } from '@/api/errors'
import { homePath, PASSWORD_TOKEN_KEY, signedInAs, type Me } from '@/auth/session'
import { AuthLayout } from '@/components/AuthLayout'
import { FormField } from '@/components/FormField'
import { ErrorNotice } from '@/components/members'
import { Button } from '@/components/ui/button'
import { formatDate } from '@/lib/format'
import { acceptInviteSchema, PASSWORD_MIN, setPasswordSchema } from '@/lib/validation'

/** `/invite/:token`: pick username (unless preset), email (unless preset) and password. */
export function AcceptInvitePage() {
  const { t } = useTranslation('auth')
  const token = useParams().token ?? ''
  const errorMessage = useApiErrorMessage()
  const client = useQueryClient()
  // Don't keep the raw token in the query cache after leaving the page.
  useEffect(() => () => client.removeQueries({ queryKey: ['invitation', token] }), [client, token])
  const preview = useQuery({
    queryKey: ['invitation', token],
    queryFn: () => unwrap(api.GET('/api/v1/invitations/{token}', { params: { path: { token } } })),
    retry: false,
    staleTime: Infinity,
  })

  if (preview.isPending) {
    return (
      <AuthLayout eyebrow={t('invite.eyebrow')} title={t('invite.title')} description="">
        <p className="text-base text-text-2" role="status">
          {t('invite.checking')}
        </p>
      </AuthLayout>
    )
  }
  if (preview.isError) {
    return (
      <AuthLayout
        eyebrow={t('invite.eyebrow')}
        title={t('invite.invalidTitle')}
        description={errorMessage(preview.error)}
      >
        <Button asChild className="self-start">
          <Link to="/login">{t('invite.goToLogin')}</Link>
        </Button>
      </AuthLayout>
    )
  }
  return <AcceptInviteForm token={token} invitation={preview.data} />
}

function AcceptInviteForm({
  token,
  invitation,
}: {
  token: string
  invitation: { presetUsername?: string | null; presetEmail?: string | null; expiresAt: string }
}) {
  const { t } = useTranslation('auth')
  const errorMessage = useApiErrorMessage()
  const [failure, setFailure] = useState<unknown>(null)
  const [me, setMe] = useState<Me | null>(null)
  const presetUsername = invitation.presetUsername ?? null
  const presetEmail = invitation.presetEmail ?? null
  const schema = acceptInviteSchema({
    username: Boolean(presetUsername),
    email: Boolean(presetEmail),
  })
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: { username: '', email: '', password: '', confirm: '' },
  })

  const onSubmit = handleSubmit(async (values) => {
    setFailure(null)
    try {
      const result = await unwrap(
        api.POST('/api/v1/invitations/{token}/accept', {
          params: { path: { token } },
          body: {
            username: presetUsername ? undefined : values.username,
            email: presetEmail ? undefined : values.email,
            password: values.password,
          },
        }),
      )
      signedInAs(result)
      setMe(result)
    } catch (error) {
      setFailure(error)
    }
  })

  if (me) return <Navigate to={homePath(me.kind)} replace />

  return (
    <AuthLayout
      eyebrow={t('invite.eyebrow')}
      title={t('invite.title')}
      description={presetUsername ? t('invite.descriptionPreset') : t('invite.description')}
    >
      <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
        {failure !== null && <ErrorNotice focus>{errorMessage(failure)}</ErrorNotice>}
        {(presetUsername || presetEmail) && (
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 rounded-card border border-line bg-night-1 px-4 py-3 text-base">
            {presetUsername && (
              <>
                <dt className="text-text-3">{t('invite.presetUsername')}</dt>
                <dd className="type-data text-text">{presetUsername}</dd>
              </>
            )}
            {presetEmail && (
              <>
                <dt className="text-text-3">{t('invite.presetEmail')}</dt>
                <dd className="text-text">{presetEmail}</dd>
              </>
            )}
          </dl>
        )}
        {!presetUsername && (
          <FormField
            label={t('fields.username')}
            error={errors.username?.message}
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            {...register('username')}
          />
        )}
        {!presetEmail && (
          <FormField
            label={t('fields.email')}
            error={errors.email?.message}
            type="email"
            autoComplete="email"
            {...register('email')}
          />
        )}
        <PasswordFields
          password={register('password')}
          confirm={register('confirm')}
          passwordError={errors.password?.message}
          confirmError={errors.confirm?.message}
        />
        <p className="text-sm text-text-3">
          {t('invite.expires', { date: formatDate(invitation.expiresAt) })}
        </p>
        <Button type="submit" variant="primary" disabled={isSubmitting} className="self-start">
          {t('invite.submit')}
        </Button>
      </form>
    </AuthLayout>
  )
}

/**
 * `/set-password/:token`: one-time link from the admin (new member or reset). The link is checked
 * on load (AUTH-03): a link that can't be used says why and what to do next before anyone types
 * a password.
 */
export function SetPasswordPage() {
  const { t } = useTranslation('auth')
  const token = useParams().token ?? ''
  const errorMessage = useApiErrorMessage()
  const client = useQueryClient()
  // Don't keep the raw token in the query cache after leaving the page.
  useEffect(
    () => () => client.removeQueries({ queryKey: [PASSWORD_TOKEN_KEY, token] }),
    [client, token],
  )
  const check = useQuery({
    queryKey: [PASSWORD_TOKEN_KEY, token],
    queryFn: () =>
      unwrap(api.GET('/api/v1/password-tokens/{token}', { params: { path: { token } } })),
    retry: false,
    staleTime: Infinity,
  })

  if (check.isPending) {
    return (
      <AuthLayout title={t('setPassword.title')} description="">
        <p className="text-base text-text-2" role="status">
          {t('setPassword.checking')}
        </p>
      </AuthLayout>
    )
  }
  if (check.isError) {
    // The link itself is the problem: say why and what to do next.
    if (errorCode(check.error).startsWith('password_token.')) {
      return (
        <AuthLayout title={t('setPassword.invalidTitle')} description={errorMessage(check.error)}>
          <p className="text-base text-text-2">{t('setPassword.nextStep')}</p>
          <Button asChild variant="primary" className="self-start">
            <Link to="/login">{t('setPassword.goToLogin')}</Link>
          </Button>
        </AuthLayout>
      )
    }
    // The check failed (throttled, offline): the link may still be fine.
    return (
      <AuthLayout title={t('setPassword.checkFailedTitle')} description={errorMessage(check.error)}>
        <div className="flex flex-wrap items-center gap-3">
          <Button
            type="button"
            variant="primary"
            disabled={check.isFetching}
            onClick={() => void check.refetch()}
          >
            {t('setPassword.retry')}
          </Button>
          <Button asChild variant="ghost">
            <Link to="/login">{t('setPassword.goToLogin')}</Link>
          </Button>
        </div>
      </AuthLayout>
    )
  }
  return <SetPasswordForm token={token} expiresAt={check.data.expiresAt} />
}

function SetPasswordForm({ token, expiresAt }: { token: string; expiresAt: string }) {
  const { t } = useTranslation('auth')
  const errorMessage = useApiErrorMessage()
  const [failure, setFailure] = useState<unknown>(null)
  const [me, setMe] = useState<Me | null>(null)
  const [pendingToken, setPendingToken] = useState<string | null>(null)
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<z.infer<typeof setPasswordSchema>>({
    resolver: zodResolver(setPasswordSchema),
    defaultValues: { password: '', confirm: '' },
  })

  const onSubmit = handleSubmit(async ({ password }) => {
    setFailure(null)
    try {
      const result = await unwrap(
        api.POST('/api/v1/password-tokens/{token}', {
          params: { path: { token } },
          body: { password },
        }),
      )
      // An admin with TOTP still needs a code: continue on the login page.
      if ('pendingToken' in result) {
        setPendingToken(result.pendingToken)
        return
      }
      signedInAs(result)
      setMe(result)
    } catch (error) {
      setFailure(error)
    }
  })

  if (pendingToken) return <Navigate to="/login" replace state={{ pendingToken }} />
  if (me) return <Navigate to={homePath(me.kind)} replace />

  return (
    <AuthLayout title={t('setPassword.title')} description={t('setPassword.description')}>
      <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
        {failure !== null && <ErrorNotice focus>{errorMessage(failure)}</ErrorNotice>}
        <PasswordFields
          password={register('password')}
          confirm={register('confirm')}
          passwordError={errors.password?.message}
          confirmError={errors.confirm?.message}
        />
        <p className="text-sm text-text-3">
          {t('setPassword.expires', { date: formatDate(expiresAt) })}
        </p>
        <Button type="submit" variant="primary" disabled={isSubmitting} className="self-start">
          {t('setPassword.submit')}
        </Button>
      </form>
    </AuthLayout>
  )
}

function PasswordFields({
  password,
  confirm,
  passwordError,
  confirmError,
}: {
  password: UseFormRegisterReturn
  confirm: UseFormRegisterReturn
  passwordError?: string
  confirmError?: string
}) {
  const { t } = useTranslation('auth')
  return (
    <>
      <FormField
        label={t('fields.password')}
        hint={t('fields.passwordHint', { count: PASSWORD_MIN })}
        error={passwordError}
        type="password"
        autoComplete="new-password"
        {...password}
      />
      <FormField
        label={t('fields.confirmPassword')}
        error={confirmError}
        type="password"
        autoComplete="new-password"
        {...confirm}
      />
    </>
  )
}
