import { CompassIcon, HourglassIcon } from 'lucide-react'
import { useCallback, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { tmdbImage } from '@/api/categories'
import { type Highlight, useDiscoverCategories } from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { Ambient } from '@/components/Ambient'
import { Hero, HeroSkeleton } from '@/components/Hero'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { EmptyState } from '@/components/EmptyState'
import { ErrorNotice } from '@/components/members'
import { Swimlane } from '@/components/Swimlane'

/** A lane's shape while the categories load: heading and a row of covers (MEM-13). */
function LaneSkeleton() {
  const { t } = useTranslation('common')
  return (
    <div aria-busy className="flex flex-col gap-4">
      <p role="status" className="sr-only">
        {t('status.loading')}
      </p>
      <div aria-hidden className="h-7 w-40 rounded-card bg-night-1 motion-safe:animate-pulse" />
      <div aria-hidden className="flex gap-4 overflow-hidden">
        {Array.from({ length: 7 }, (_, i) => (
          <div
            key={i}
            className="aspect-[2/3] w-36 shrink-0 rounded-card bg-night-1 motion-safe:animate-pulse sm:w-44"
          />
        ))}
      </div>
    </div>
  )
}

/**
 * Discover (spec 0008 W2): the hero "Bühne" with the mixed top 10, then a lane per category the
 * member may browse. Colour enters only from the hero's current title, as the ambient layer
 * behind the page (A1). The page's heading is for screen readers; the hero is the stage.
 */
export default function DiscoverPage() {
  const { t } = useTranslation(['member', 'common'])
  const categories = useDiscoverCategories()
  const errorMessage = useApiErrorMessage()
  const { open } = useTitleSheet()
  const ready = categories.data?.filter((c) => c.discoverReady) ?? []
  const [ambient, setAmbient] = useState<string | null>(null)
  const onActive = useCallback(
    (item: Highlight | null) =>
      setAmbient(item?.backdrop ? tmdbImage(item.backdrop, 'w300') : null),
    [],
  )

  return (
    <div className="relative isolate">
      <Ambient
        src={ambient}
        className="-inset-x-4 -top-8 h-[64rem] [mask-image:linear-gradient(#000_45%,transparent)] [mask-repeat:no-repeat] sm:-inset-x-10 lg:-top-[140px]"
      />
      <h1 className="sr-only">{t('discover.title')}</h1>
      {categories.isError && <ErrorNotice>{errorMessage(categories.error)}</ErrorNotice>}
      {categories.isPending && (
        <>
          <HeroSkeleton />
          <div className="mt-10 lg:mt-12">
            <LaneSkeleton />
          </div>
        </>
      )}
      {categories.data?.length === 0 && (
        <EmptyState
          icon={CompassIcon}
          title={t('discover.emptyTitle')}
          description={t('discover.emptyDescription')}
        />
      )}
      {categories.data && categories.data.length > 0 && ready.length === 0 && (
        <EmptyState
          icon={HourglassIcon}
          title={t('discover.notReadyTitle')}
          description={t('discover.notReadyDescription')}
        />
      )}
      {ready.length > 0 && (
        <>
          <Hero onOpen={open} onActiveChange={onActive} />
          <div className="mt-10 flex flex-col gap-12 lg:mt-12">
            {ready.map((category) => (
              <Swimlane key={category.category} category={category} onOpen={open} />
            ))}
          </div>
        </>
      )}
    </div>
  )
}
