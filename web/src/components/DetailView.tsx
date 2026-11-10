import { XIcon } from 'lucide-react'
import { Dialog as DialogPrimitive } from 'radix-ui'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import type { TitleRef } from '@/api/discover'
import { Ambient } from '@/components/Ambient'
import { AlbumDetail, ArtistDetail } from '@/components/detail/MusicDetail'
import { NotFound } from '@/components/detail/states'
import { TitleDetail } from '@/components/detail/TitleDetail'
import { type DetailStage, DetailStageContext } from '@/lib/detailStage'
import { useReturnFocus } from '@/lib/returnFocus'
import { titleRefParam } from '@/lib/seasons'
import { useTitleSheet } from '@/lib/useTitleSheet'
import { cn } from '@/lib/utils'

/*
 * The detail overlay (spec 0008 A7, Q4 B, Q5 B; ADR 0015): one modal dialog for movies, series,
 * albums and artists, opened by `?title=<kind>-<id>` over whatever page is showing. Back or Esc
 * closes it and focus goes back to the poster that opened it (the page heading after a deep
 * link). Below 1024 px it fills the screen. The page underneath keeps its scroll and lanes.
 */

/** Renders the detail overlay for whatever `?title=` names (member shell). */
export function DetailViewHost() {
  const { ref, raw, close } = useTitleSheet()
  return <DetailView titleRef={ref} open={raw !== null} onClose={close} />
}

export function DetailView({
  titleRef,
  open,
  onClose,
}: {
  /** What `?title=` names; null when it names nothing valid ("not found", MEM-32). */
  titleRef: TitleRef | null
  open: boolean
  onClose: () => void
}) {
  const { t } = useTranslation('common')
  const focus = useReturnFocus()
  const [ambient, setAmbient] = useState<string | null>(null)
  const [memory] = useState(() => new Map<string, unknown>())
  // A stable stage: the host hands a new `onClose` each render, the views only need to call it.
  const closeRef = useRef(onClose)
  useEffect(() => {
    closeRef.current = onClose
  }, [onClose])
  const [stage] = useState<DetailStage>(() => ({
    setAmbient,
    memory,
    close: () => closeRef.current(),
  }))
  // The opener is taken before any effect of the view runs: with cached data the tabs take
  // focus in their own effect, Radix then sees focus inside and never calls `onOpenAutoFocus`.
  const { capture } = focus
  useLayoutEffect(() => {
    if (open) capture()
  }, [open, capture])
  // Nothing of a closed overlay lingers into the next one.
  useEffect(() => {
    if (!open) memory.clear()
  }, [open, memory])

  return (
    <DialogPrimitive.Root open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="detail-overlay fixed inset-0 z-50 isolate bg-night-0/85">
          <Ambient src={ambient} className="inset-0" />
        </DialogPrimitive.Overlay>
        <DialogPrimitive.Content
          aria-describedby={undefined}
          // Focus starts on the dialog; once the tabs are there the first one takes it (A7).
          onOpenAutoFocus={(event) => {
            event.preventDefault()
            const dialog = event.currentTarget
            if (!(dialog instanceof HTMLElement)) return
            // Cached data: the tabs are already there.
            const tab = dialog.querySelector<HTMLElement>('[role="tab"][data-state="active"]')
            ;(tab ?? dialog).focus()
          }}
          onCloseAutoFocus={focus.onCloseAutoFocus}
          className={cn(
            'detail-dialog fixed inset-0 z-50 overflow-y-auto overscroll-contain bg-night-1 outline-none',
            'lg:inset-x-6 lg:inset-y-6 lg:mx-auto lg:max-w-[70rem] lg:rounded-panel lg:border lg:border-line',
          )}
        >
          <DetailStageContext.Provider value={stage}>
            {/* Sticky, so the way out stays in reach however far the view scrolls. */}
            <div className="sticky top-0 z-30 h-0">
              <DialogPrimitive.Close className="absolute top-3 right-3 grid size-11 cursor-pointer place-items-center rounded-full bg-night-0/85 text-text-2 transition-colors duration-(--dur-1) hover:text-text lg:top-4 lg:right-4">
                <XIcon aria-hidden className="size-5" />
                <span className="sr-only">{t('actions.close')}</span>
              </DialogPrimitive.Close>
            </div>
            {titleRef ? (
              <DetailBody key={titleRefParam(titleRef)} titleRef={titleRef} />
            ) : (
              <NotFound />
            )}
          </DetailStageContext.Provider>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  )
}

function DetailBody({ titleRef }: { titleRef: TitleRef }) {
  switch (titleRef.kind) {
    case 'album':
      return <AlbumDetail id={titleRef.id} />
    case 'artist':
      return <ArtistDetail id={titleRef.id} />
    default:
      return <TitleDetail titleRef={titleRef} />
  }
}
