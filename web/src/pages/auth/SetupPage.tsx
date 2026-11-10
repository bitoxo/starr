import { zodResolver } from '@hookform/resolvers/zod'
import { CircleIcon } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { z } from 'zod'

import type { components } from '@/api/schema'
import { AuthError, setUpAdmin, type AuthErrorCode } from '@/auth/session'
import { AuthLayout } from '@/components/AuthLayout'
import { FormField } from '@/components/FormField'
import { ErrorNotice } from '@/components/members'
import { Button } from '@/components/ui/button'
import { newPassword, PASSWORD_MIN, required, username } from '@/lib/validation'

const schema = z.object({
  setupToken: required,
  username,
  password: newPassword,
})

type SetupValues = z.infer<typeof schema>

type SetupErrorKey =
  | 'errors.setupInvalidToken'
  | 'errors.setupCompleted'
  | 'errors.setupPublicUrl'
  | 'errors.usernameTaken'
  | 'errors.unavailable'

const ERROR_KEY: Record<string, SetupErrorKey> = {
  'setup.invalid_token': 'errors.setupInvalidToken',
  'setup.already_completed': 'errors.setupCompleted',
  'setup.public_url_required': 'errors.setupPublicUrl',
  'validation.username_taken': 'errors.usernameTaken',
}

/** The Inbox's first-run checklist (spec 0008 slice 9), in its order. */
const NEXT_STEPS: components['schemas']['SetupStepKind'][] = [
  'services',
  'categories',
  'two_step',
  'members',
]

export default function SetupPage() {
  const { t } = useTranslation(['auth', 'admin'])
  const [failure, setFailure] = useState<AuthErrorCode | null>(null)
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<SetupValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      setupToken: '',
      username: '',
      password: '',
    },
  })

  // On success the session is set and GuestOnly opens the Inbox's setup checklist.
  const onSubmit = handleSubmit(async ({ setupToken, username, password }) => {
    setFailure(null)
    try {
      await setUpAdmin({ token: setupToken.trim(), username, password })
    } catch (error) {
      setFailure(error instanceof AuthError ? error.code : 'unavailable')
    }
  })

  return (
    <AuthLayout
      eyebrow={t('setup.eyebrow')}
      title={t('setup.title')}
      description={t('setup.description')}
    >
      <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
        {failure && (
          <ErrorNotice focus>{t(ERROR_KEY[failure] ?? 'errors.unavailable')}</ErrorNotice>
        )}
        <FormField
          label={t('setup.token')}
          hint={t('setup.tokenHint')}
          error={errors.setupToken?.message}
          autoComplete="off"
          spellCheck={false}
          className="type-data"
          {...register('setupToken')}
        />
        <FormField
          label={t('fields.username')}
          hint={t('setup.usernameHint')}
          error={errors.username?.message}
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          {...register('username')}
        />
        <FormField
          label={t('fields.password')}
          hint={t('fields.passwordHint', { count: PASSWORD_MIN })}
          error={errors.password?.message}
          type="password"
          autoComplete="new-password"
          {...register('password')}
        />
        <Button type="submit" variant="primary" disabled={isSubmitting} className="mt-2 self-start">
          {t('setup.submit')}
        </Button>
      </form>
      <section
        aria-labelledby="setup-next"
        className="flex flex-col gap-2 border-t border-line pt-6"
      >
        <h2 id="setup-next" className="text-base font-semibold text-text">
          {t('setup.nextTitle')}
        </h2>
        <p className="text-sm text-text-2">{t('setup.nextDescription')}</p>
        <ul className="mt-1 grid gap-1.5 text-sm text-text-2">
          {NEXT_STEPS.map((step) => (
            <li key={step} className="flex items-center gap-2.5">
              <CircleIcon aria-hidden className="size-4 text-text-3" />
              {t(`admin:inbox.setup.step.${step}`)}
            </li>
          ))}
        </ul>
      </section>
    </AuthLayout>
  )
}
