import { ShieldIcon } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { useSession } from '@/auth/session'
import { LazyBackToAdminDialog } from '@/components/LazyBackToAdminDialog'
import { Button } from '@/components/ui/button'

/**
 * While an admin looks at Starr through their member profile, a strip on every member page says
 * so and leads back (spec 0008 MEM-19). Nothing is rendered for ordinary members.
 */
export function MemberViewStrip() {
  const { t } = useTranslation()
  const switchedFrom = useSession().me?.switchedFrom
  const [open, setOpen] = useState(false)
  if (!switchedFrom) return null
  return (
    <section
      aria-label={t('memberView.label')}
      data-member-view
      className="flex items-center gap-3 border-b border-line bg-night-2 py-1 pr-2 pl-4 text-sm text-text-2"
    >
      <ShieldIcon aria-hidden className="size-4 shrink-0 text-text-3" strokeWidth={1.75} />
      <p className="min-w-0 flex-1">
        <span className="font-semibold text-text">{t('memberView.title')}</span>
        <span className="max-sm:sr-only"> · {t('memberView.description')}</span>
      </p>
      <Button variant="ghost" size="sm" className="max-lg:h-11" onClick={() => setOpen(true)}>
        {t('memberView.back')}
      </Button>
      <LazyBackToAdminDialog
        open={open}
        onOpenChange={setOpen}
        adminName={switchedFrom.displayName}
        totpEnabled={switchedFrom.totpEnabled}
      />
    </section>
  )
}
