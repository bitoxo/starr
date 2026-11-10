import { zodResolver } from '@hookform/resolvers/zod'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Link, useLocation } from 'react-router'
import { z } from 'zod'

import { AuthError, completeTotpSignIn, signIn, type AuthErrorCode } from '@/auth/session'
import { FormField } from '@/components/FormField'
import { ErrorNotice } from '@/components/members'
import { Button } from '@/components/ui/button'
import { password, required, secondFactor } from '@/lib/validation'

import { LoginLayout } from './LoginLayout'

const schema = z.object({ username: required, password })
type LoginValues = z.infer<typeof schema>

type ErrorKey =
  | 'errors.invalidCredentials'
  | 'errors.tooManyAttempts'
  | 'errors.invalidCode'
  | 'errors.loginExpired'

const ERROR_KEY: Record<string, ErrorKey> = {
  'auth.invalid_credentials': 'errors.invalidCredentials',
  'auth.too_many_attempts': 'errors.tooManyAttempts',
  'auth.invalid_code': 'errors.invalidCode',
  'auth.login_expired': 'errors.loginExpired',
}

/**
 * Why signing in failed. It takes focus when it appears (AUTH-04), so screen readers hear it and
 * focus never drops to `<body>`; the next Tab goes on to the fields. The expired-login notice on
 * arrival keeps focus where the page puts it.
 */
function Failure({ code, focus = true }: { code: AuthErrorCode; focus?: boolean }) {
  const { t } = useTranslation('auth')
  return <ErrorNotice focus={focus}>{t(ERROR_KEY[code] ?? 'errors.unavailable')}</ErrorNotice>
}

export default function LoginPage() {
  // A set-password link for an admin with TOTP lands here with a pending login.
  const fromLink = (useLocation().state as { pendingToken?: string } | null)?.pendingToken
  const [pendingToken, setPendingToken] = useState<string | null>(fromLink ?? null)
  const [expired, setExpired] = useState(false)

  if (pendingToken) {
    return (
      <CodeStep
        pendingToken={pendingToken}
        onBack={() => {
          setPendingToken(null)
          setExpired(false)
        }}
        onExpired={() => {
          setPendingToken(null)
          setExpired(true)
        }}
      />
    )
  }
  return <PasswordStep expired={expired} onTotp={setPendingToken} />
}

function PasswordStep({
  expired,
  onTotp,
}: {
  expired: boolean
  onTotp: (pendingToken: string) => void
}) {
  const { t } = useTranslation('auth')
  const [failure, setFailure] = useState<AuthErrorCode | null>(
    expired ? 'auth.login_expired' : null,
  )
  // The notice for an expired second step is shown on arrival; failures of this form take focus.
  const [submitted, setSubmitted] = useState(false)
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<LoginValues>({
    resolver: zodResolver(schema),
    defaultValues: { username: '', password: '' },
  })

  const onSubmit = handleSubmit(async ({ username, password }) => {
    setFailure(null)
    setSubmitted(true)
    try {
      const result = await signIn(username, password)
      if (result.kind === 'totp') onTotp(result.pendingToken)
    } catch (error) {
      setFailure(error instanceof AuthError ? error.code : 'unavailable')
    }
  })

  return (
    <LoginLayout title={t('login.title')} description={t('login.description')}>
      <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
        {failure && <Failure code={failure} focus={submitted} />}
        <FormField
          label={t('fields.username')}
          error={errors.username?.message}
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          autoFocus
          {...register('username')}
        />
        <FormField
          label={t('fields.password')}
          error={errors.password?.message}
          type="password"
          autoComplete="current-password"
          {...register('password')}
        />
        <div className="mt-2 flex flex-wrap items-center gap-x-6 gap-y-3">
          <Button type="submit" variant="primary" disabled={isSubmitting}>
            {t('login.submit')}
          </Button>
          <Link
            to="/forgot-password"
            className="inline-flex min-h-6 items-center rounded-control text-sm font-medium text-text-2 underline-offset-4 hover:text-text hover:underline max-md:min-h-11"
          >
            {t('login.forgot')}
          </Link>
        </div>
      </form>
    </LoginLayout>
  )
}

const codeSchema = z.object({ code: secondFactor })

/** Second step for admins with TOTP: a code from the app or a recovery code. */
export function CodeStep({
  pendingToken,
  onBack,
  onExpired,
}: {
  pendingToken: string
  onBack: () => void
  onExpired: () => void
}) {
  const { t } = useTranslation('auth')
  const [failure, setFailure] = useState<AuthErrorCode | null>(null)
  const {
    register,
    handleSubmit,
    resetField,
    formState: { errors, isSubmitting },
  } = useForm<z.infer<typeof codeSchema>>({
    resolver: zodResolver(codeSchema),
    defaultValues: { code: '' },
  })

  const onSubmit = handleSubmit(async ({ code }) => {
    setFailure(null)
    try {
      await completeTotpSignIn(pendingToken, code)
    } catch (error) {
      const code = error instanceof AuthError ? error.code : 'unavailable'
      if (code === 'auth.login_expired') {
        onExpired()
        return
      }
      setFailure(code)
      resetField('code')
    }
  })

  return (
    <LoginLayout title={t('totpLogin.title')} description={t('totpLogin.description')}>
      <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
        {failure && <Failure code={failure} />}
        <FormField
          label={t('fields.code')}
          hint={t('totpLogin.hint')}
          error={errors.code?.message}
          autoComplete="one-time-code"
          inputMode="text"
          autoCapitalize="none"
          spellCheck={false}
          autoFocus
          className="type-data tracking-widest"
          {...register('code')}
        />
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <Button type="submit" variant="primary" disabled={isSubmitting}>
            {t('totpLogin.submit')}
          </Button>
          <Button type="button" variant="ghost" onClick={onBack}>
            {t('totpLogin.back')}
          </Button>
        </div>
      </form>
    </LoginLayout>
  )
}
