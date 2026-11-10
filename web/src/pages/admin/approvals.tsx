import { useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import type { DecisionResult } from '@/api/attention'
import { api } from '@/api/client'
import { unwrap, useApiErrorMessage } from '@/api/errors'
import { type AdminRequest, refOf, unitsLabel } from '@/api/requests'
import { ATTENTION_KEY } from '@/lib/attention'
import { PRIVATE_TOAST_PREFIX } from '@/lib/privateGate'
import { showUndoToast } from '@/lib/undo'

/** "Severance S02", "Radiohead – OK Computer": what a toast calls a request. */
export function requestLabel(request: AdminRequest): string {
  const units = unitsLabel(request.items)
  if (!units) return request.title
  return request.category === 'music' ? `${request.title} – ${units}` : `${request.title} ${units}`
}

/**
 * What an admin hears after approving (spec 0008 ADM-30, F-4): the title and, when the source's
 * writes are paused, that it waits ("Approved: X – held until Lidarr resumes"). Undo is offered
 * only while nothing went to the source (`undoable`); a decline never gets one (0003 D11).
 * Private requests get a neutral text and a `private:` toast id (F2).
 */
export function useApprovalFeedback() {
  const { t } = useTranslation('admin')
  const client = useQueryClient()
  const errorMessage = useApiErrorMessage()
  return (requests: AdminRequest[], result: DecisionResult) => {
    const decided = requests.filter((r) => result.decided.includes(r.id))
    if (result.skipped.length > 0) {
      toast(t('inbox.skipped', { count: result.skipped.length }))
    }
    if (decided.length === 0) return
    const isPrivate = decided.some((r) => r.category === 'adult')
    const heldOf = (r: AdminRequest) => result.held.find((h) => h.requestId === r.id)
    let message: string
    const [only] = decided
    if (decided.length === 1 && only) {
      const held = heldOf(only)
      const source = held?.source ?? '–'
      if (isPrivate) {
        message = held ? t('inbox.approvedPrivateHeld', { source }) : t('inbox.approvedPrivate')
      } else {
        message = held
          ? t('inbox.approvedHeld', { title: requestLabel(only), source })
          : t('inbox.approved', { title: requestLabel(only) })
      }
    } else {
      message =
        result.held.length > 0
          ? t('inbox.approvedManyHeld', { count: decided.length })
          : t('inbox.approvedMany', { count: decided.length })
    }
    const id = isPrivate
      ? `${PRIVATE_TOAST_PREFIX}approved:${decided.map((r) => r.id).join(',')}`
      : undefined
    if (!result.undoable) {
      toast(message, { id })
      return
    }
    showUndoToast({
      id,
      message,
      onUndo: async () => {
        try {
          await unwrap(
            api.POST('/api/v1/admin/approvals/undo', {
              body: { requests: decided.map(refOf) },
            }),
          )
          toast(t('inbox.undone'), { id: id && `${id}:undone` })
        } catch (error) {
          toast.error(errorMessage(error))
        } finally {
          void client.invalidateQueries({ queryKey: ['requests'] })
          void client.invalidateQueries({ queryKey: ATTENTION_KEY })
        }
      },
    })
  }
}
