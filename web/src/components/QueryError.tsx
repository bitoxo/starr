import { TriangleAlertIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import { TechnicalDetails } from '@/components/TechnicalDetails'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

/**
 * A failed load (spec 0008 F10): one plain sentence with the next step and a retry, never an
 * endless "Loading…". Raw upstream text, if any, sits behind "Technical details".
 */
export function QueryError({
  message,
  detail,
  onRetry,
  className,
}: {
  /** What went wrong, in words, with what to do next. */
  message?: string
  /** The service's own text, for admins and bug reports. */
  detail?: string | null
  onRetry?: () => void
  className?: string
}) {
  const { t } = useTranslation()
  return (
    <div
      role="alert"
      className={cn(
        'flex flex-col items-start gap-3 rounded-card border border-dashed border-line px-5 py-6',
        className,
      )}
    >
      <p className="flex items-start gap-2 text-base text-text-2">
        <TriangleAlertIcon aria-hidden className="mt-1 size-4 shrink-0 text-warn" />
        {message ?? t('status.error')}
      </p>
      {detail && <TechnicalDetails text={detail} />}
      {onRetry && (
        <Button size="sm" onClick={onRetry}>
          {t('actions.retry')}
        </Button>
      )}
    </div>
  )
}
