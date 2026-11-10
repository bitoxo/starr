import type { ComponentProps } from 'react'
import { useTranslation } from 'react-i18next'

import { cn } from '@/lib/utils'

/**
 * The mark: star trails circling Polaris (docs/design/logo/starr-mark.svg).
 * Trails take the current text colour; Polaris always stays gold. Minimum 16 px.
 */
export function LogoMark({ className, ...props }: ComponentProps<'svg'>) {
  return (
    <svg
      viewBox="0 0 64 64"
      aria-hidden
      focusable="false"
      className={cn('size-8 shrink-0 text-text', className)}
      {...props}
    >
      <g fill="none" stroke="currentColor" strokeWidth="4.2" strokeLinecap="round">
        <path d="M22.03 27.35A11 11 0 0 1 42.96 31.04" />
        <path d="M25.67 49.38A18.5 18.5 0 1 1 47.15 21.39" />
        <path d="M23.11 7.57A26 26 0 0 1 57.90 34.27" />
      </g>
      <circle cx="32" cy="32" r="4.6" className="fill-polaris" />
    </svg>
  )
}

/** Mark plus the lowercase wordmark, set as text in Bricolage 800 / wdth 78. */
export function Logo({ className, markClassName }: { className?: string; markClassName?: string }) {
  const { t } = useTranslation()
  return (
    <span className={cn('inline-flex items-center gap-2.5 text-text', className)}>
      <LogoMark className={markClassName} />
      <span className="wordmark text-2xl leading-none">{t('app.wordmark')}</span>
    </span>
  )
}
