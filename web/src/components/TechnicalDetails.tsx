import { useTranslation } from 'react-i18next'

import { cn } from '@/lib/utils'

/**
 * A service's or the worker's own text, collapsed under the plain sentence that explains it
 * (spec 0008 F10, ADM-17): for admins and bug reports, never the primary information.
 */
export function TechnicalDetails({ text, className }: { text: string; className?: string }) {
  const { t } = useTranslation()
  return (
    <details className={cn('max-w-full text-sm text-text-3', className)}>
      <summary className="cursor-pointer rounded-control text-text-2 hover:text-text">
        {t('status.technicalDetails')}
      </summary>
      <pre className="type-data mt-2 max-h-48 overflow-auto rounded-control border border-line bg-night-0 p-3 text-xs break-words whitespace-pre-wrap">
        {text}
      </pre>
    </details>
  )
}
