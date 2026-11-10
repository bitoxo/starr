import { useContext, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { UNSAFE_DataRouterContext, useBlocker } from 'react-router'

import { ConfirmDialog } from '@/components/ConfirmDialog'

/**
 * Warns before unsaved changes are lost (spec 0008 F8, ADM-11): closing or reloading the tab
 * gets the browser's prompt, and leaving the page inside Starr asks first. A form renders it
 * with `when={isDirty}` and keeps Save disabled while clean. React Router allows one blocker at
 * a time: render one guard per page (combine the forms' dirty states). After saving, clear
 * `when` before navigating away in `onSuccess`, or the guard asks.
 */
export function UnsavedChangesGuard({ when }: { when: boolean }) {
  useEffect(() => {
    if (!when) return
    const warn = (event: BeforeUnloadEvent) => {
      event.preventDefault()
      // Older Safari and Chrome only prompt with a return value set.
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', warn)
    return () => window.removeEventListener('beforeunload', warn)
  }, [when])
  // In-app navigation can only be held in a data router (the app's); plain routers (tests,
  // embedded pages) still get the tab prompt.
  const dataRouter = useContext(UNSAFE_DataRouterContext)
  return dataRouter ? <HoldNavigation when={when} /> : null
}

function HoldNavigation({ when }: { when: boolean }) {
  const { t } = useTranslation()
  const blocker = useBlocker(
    ({ currentLocation, nextLocation }) =>
      when &&
      (currentLocation.pathname !== nextLocation.pathname ||
        currentLocation.search !== nextLocation.search),
  )
  return (
    <ConfirmDialog
      open={blocker.state === 'blocked'}
      onOpenChange={(open) => {
        if (!open) blocker.reset?.()
      }}
      title={t('unsaved.title')}
      description={t('unsaved.description')}
      confirmLabel={t('unsaved.leave')}
      onConfirm={() => blocker.proceed?.()}
    />
  )
}
