import { useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { useApiErrorMessage } from '@/api/errors'
import {
  type AdminRequest,
  REASON_MAX,
  refOf,
  useRejectRequests,
  useRevokeRequests,
} from '@/api/requests'
import { RequiredMark } from '@/components/FormField'
import { ErrorNotice } from '@/components/members'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { cn } from '@/lib/utils'

/**
 * Reject (D6) or revoke (spec 0004 R3) one or more requests with a reason the
 * members read.
 */
export function ReasonDialog({
  action,
  requests,
  onDone,
}: {
  action: 'reject' | 'revoke'
  requests: AdminRequest[] | null
  onDone: (decided: boolean) => void
}) {
  const { t } = useTranslation(['admin', 'common'])
  const rejectRequests = useRejectRequests()
  const revokeRequests = useRevokeRequests()
  const reject = action === 'reject' ? rejectRequests : revokeRequests
  const dialog = action === 'reject' ? 'needsYou.rejectDialog' : 'needsYou.revokeDialog'
  const errorMessage = useApiErrorMessage()
  const [reason, setReason] = useState('')
  // Shown after a submit without a reason (ADM-36), until one is typed.
  const [missing, setMissing] = useState(false)
  const fieldId = useId()
  const errorId = `${fieldId}-error`
  const field = useRef<HTMLTextAreaElement>(null)
  const trimmed = reason.trim()
  // Characters as the server counts them (code points, not UTF-16 units).
  const length = [...trimmed].length
  const valid = length > 0 && length <= REASON_MAX
  const count = requests?.length ?? 0

  const close = (rejected: boolean) => {
    setReason('')
    setMissing(false)
    reject.reset()
    onDone(rejected)
  }

  return (
    <Dialog open={requests !== null} onOpenChange={(open) => !open && close(false)}>
      <DialogContent className="max-w-lg">
        <form
          noValidate
          className="flex flex-col gap-5"
          onSubmit={(e) => {
            e.preventDefault()
            if (!requests) return
            if (!valid) {
              setMissing(true)
              field.current?.focus()
              return
            }
            const body = { requests: requests.map(refOf), reason: trimmed }
            if (action === 'reject') {
              rejectRequests.mutate(body, {
                onSuccess: (result) => {
                  toast(t('needsYou.rejected', { count: result.decided.length }))
                  close(true)
                },
              })
            } else {
              revokeRequests.mutate(body, {
                onSuccess: (result) => {
                  toast(t('needsYou.revoked', { count: result.revoked.length }))
                  if (result.skipped.length > 0) {
                    toast(t('needsYou.revokeSkipped', { count: result.skipped.length }))
                  }
                  close(true)
                },
              })
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>
              {count === 1 && requests?.[0]
                ? t(`${dialog}.titleOne`, { title: requests[0].title })
                : t(`${dialog}.titleMany`, { count })}
            </DialogTitle>
            <DialogDescription>{t(`${dialog}.description`)}</DialogDescription>
          </DialogHeader>
          <DialogBody>
            {reject.isError && <ErrorNotice>{errorMessage(reject.error)}</ErrorNotice>}
            <div className="flex flex-col gap-1.5">
              <label htmlFor={fieldId} className="text-sm font-medium text-text-2">
                {t(`${dialog}.reason`)}
                <RequiredMark />
              </label>
              <textarea
                ref={field}
                id={fieldId}
                value={reason}
                aria-invalid={missing || undefined}
                aria-describedby={missing ? errorId : undefined}
                onChange={(e) => {
                  setReason(e.target.value)
                  if (e.target.value.trim()) setMissing(false)
                }}
                rows={3}
                maxLength={REASON_MAX}
                required
                placeholder={t(`${dialog}.placeholder`)}
                className={cn(
                  'w-full min-w-0 resize-y rounded-control border border-line bg-night-0 px-3 py-2 text-base text-text transition-colors outline-hidden placeholder:text-text-3',
                  'hover:border-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-0 focus-visible:outline-polaris/40',
                )}
              />
              <span className="flex items-start justify-between gap-3">
                {missing ? (
                  <span id={errorId} className="text-sm text-bad">
                    {t('needsYou.reasonMissing')}
                  </span>
                ) : (
                  <span />
                )}
                <span className="type-data text-xs text-text-3">
                  {length}/{REASON_MAX}
                </span>
              </span>
            </div>
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => close(false)}>
              {t('common:actions.cancel')}
            </Button>
            {/* Declining and revoking are destructive: a bad outline, never gold (ADM-16). */}
            <Button
              type="submit"
              className="border-bad/60 text-bad hover:border-bad"
              disabled={reject.isPending}
            >
              {t(`${dialog}.submit`, { count })}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
