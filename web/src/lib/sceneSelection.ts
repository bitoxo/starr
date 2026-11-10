import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { MAX_SCENES } from '@/api/private'
import { privateToast } from '@/lib/privateGate'

/*
 * Multi-select of private scenes (spec 0008 PRIV-18): pick up to 20 not-yet-requested scenes to
 * request together; Esc clears the selection unless a dialog or menu is open.
 */

/** The selection of scenes to request together: at most {@link MAX_SCENES}, Esc clears. */
export function useSceneSelection(resetOn = '') {
  const { t } = useTranslation('member')
  const [selected, setSelected] = useState<string[]>([])
  // Another search, filter or page: scenes picked for what was shown before go.
  const [shown, setShown] = useState(resetOn)
  if (shown !== resetOn) {
    setShown(resetOn)
    setSelected([])
  }
  const toggle = (id: string) => {
    if (selected.includes(id)) setSelected(selected.filter((s) => s !== id))
    else if (selected.length >= MAX_SCENES) privateToast(t('private.tooMany', { max: MAX_SCENES }))
    else setSelected([...selected, id])
  }
  const count = selected.length
  useEffect(() => {
    if (count === 0) return
    const onKey = (event: globalThis.KeyboardEvent) => {
      // A dialog or menu handles its own Esc first.
      if (event.key !== 'Escape' || event.defaultPrevented) return
      if (document.querySelector('[role="dialog"],[role="alertdialog"],[role="menu"]')) return
      setSelected([])
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [count])
  return { selected, toggle, clear: () => setSelected([]) }
}

export type SceneSelection = ReturnType<typeof useSceneSelection>
