import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'

import { Button } from '@/components/ui/button'

import { LoginLayout } from './LoginLayout'

/**
 * `/forgot-password` (spec 0008 AUTH-01): Starr sends no reset emails, so this page says how a
 * reset works instead: the admin makes a set-password link. Admins locked out get the recovery
 * command (ADR 0005, operator recovery).
 */
export default function ForgotPasswordPage() {
  const { t } = useTranslation('auth')
  const steps = [t('forgot.step1'), t('forgot.step2'), t('forgot.step3')]
  return (
    <LoginLayout title={t('forgot.title')} description={t('forgot.description')}>
      <section aria-labelledby="forgot-steps" className="flex flex-col gap-3">
        <h2 id="forgot-steps" className="text-base font-semibold text-text">
          {t('forgot.stepsTitle')}
        </h2>
        <ol className="flex flex-col gap-2.5 text-base text-text-2">
          {steps.map((step, i) => (
            <li key={step} className="flex gap-3">
              <span aria-hidden className="type-data w-4 shrink-0 text-text-3">
                {i + 1}
              </span>
              <span>{step}</span>
            </li>
          ))}
        </ol>
      </section>
      <div className="flex flex-col gap-2 rounded-card border border-line bg-night-1 px-4 py-3 text-sm text-text-2">
        <p>{t('forgot.admins')}</p>
        <code className="type-data break-all text-text">starr admin recover &lt;username&gt;</code>
      </div>
      <Button asChild variant="primary" className="self-start">
        <Link to="/login">{t('forgot.back')}</Link>
      </Button>
    </LoginLayout>
  )
}
