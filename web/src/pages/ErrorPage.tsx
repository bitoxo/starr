import { CompassIcon, TriangleAlertIcon } from 'lucide-react'
import { useEffect, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useRouteError } from 'react-router'

import { homePath, useSession } from '@/auth/session'
import { EmptyState } from '@/components/EmptyState'
import { Logo } from '@/components/Logo'
import { Button } from '@/components/ui/button'

function Frame({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-dvh flex-col gap-10 px-4 py-6 sm:px-8">
      <Logo markClassName="size-7" />
      <main className="mx-auto w-full max-w-2xl">{children}</main>
    </div>
  )
}

/**
 * "Not found" inside the shell (spec 0008 PRIV-03, MEM-31): every unknown URL and every private
 * URL the viewer may not see renders exactly this markup, so nothing tells them apart.
 */
export function NotFound() {
  const { t } = useTranslation('common')
  const session = useSession()
  return (
    <EmptyState
      icon={CompassIcon}
      title={t('errors.notFoundTitle')}
      description={t('errors.notFoundDescription')}
      action={
        <Button asChild variant="primary">
          <Link to={homePath(session.role)}>{t('actions.goHome')}</Link>
        </Button>
      }
    />
  )
}

export function ErrorPage() {
  const { t } = useTranslation()
  const error = useRouteError()
  useEffect(() => console.error(error), [error])
  return (
    <Frame>
      <EmptyState
        icon={TriangleAlertIcon}
        title={t('errors.unexpectedTitle')}
        description={t('errors.unexpectedDescription')}
        action={
          <Button variant="primary" onClick={() => window.location.reload()}>
            {t('actions.reload')}
          </Button>
        }
      />
    </Frame>
  )
}
