import { zodResolver } from '@hookform/resolvers/zod'
import { useQueryClient } from '@tanstack/react-query'
import { ShieldIcon } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import { elevate } from '@/api/security'
import { ME_KEY, useSession } from '@/auth/session'
import { FormField } from '@/components/FormField'
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
import { password, secondFactor } from '@/lib/validation'

const withCode = z.object({ password, code: secondFactor })
const withoutCode = z.object({ password, code: z.string() })
type Values = z.infer<typeof withCode>

/**
 * Re-authentication for actions that need it (spec 0005 M10 2). Used through
 * `useElevation` (`@/lib/elevation`), which opens it on
 * `auth.elevation_required` and runs the action again afterwards.
 */
export function ElevationDialog({
  open,
  onCancel,
  onElevated,
}: {
  open: boolean
  onCancel: () => void
  onElevated: () => Promise<void> | void
}) {
  const { t } = useTranslation()
  const errorMessage = useApiErrorMessage()
  const totpEnabled = useSession().me?.totpEnabled ?? false
  const client = useQueryClient()
  const {
    register,
    handleSubmit,
    reset,
    setError,
    clearErrors,
    formState: { errors, isSubmitting },
  } = useForm<Values>({
    resolver: zodResolver(totpEnabled ? withCode : withoutCode),
    defaultValues: { password: '', code: '' },
  })

  const cancel = () => {
    if (isSubmitting) return
    reset()
    onCancel()
  }

  const onSubmit = handleSubmit(async ({ password, code }) => {
    clearErrors('root')
    try {
      await elevate(totpEnabled ? { password, code } : { password })
    } catch (error) {
      setError('root', { message: errorMessage(error) })
      return
    }
    reset()
    // `/auth/me` carries `elevatedUntil`: every elevation refreshes it (ADM-34).
    void client.invalidateQueries({ queryKey: ME_KEY })
    await onElevated()
  })

  return (
    <Dialog open={open} onOpenChange={(next) => !next && cancel()}>
      <DialogContent>
        <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
          <DialogHeader>
            <DialogTitle>{t('elevation.title')}</DialogTitle>
            <DialogDescription>
              {t(totpEnabled ? 'elevation.descriptionTotp' : 'elevation.description')}
            </DialogDescription>
          </DialogHeader>
          <DialogBody>
            {errors.root?.message && <ErrorNotice>{errors.root.message}</ErrorNotice>}
            <FormField
              label={t('elevation.password')}
              error={errors.password?.message}
              type="password"
              autoComplete="current-password"
              autoFocus
              {...register('password')}
            />
            {totpEnabled && (
              <FormField
                label={t('elevation.code')}
                error={errors.code?.message}
                autoComplete="one-time-code"
                className="type-data tracking-widest"
                {...register('code')}
              />
            )}
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={cancel}>
              {t('actions.cancel')}
            </Button>
            <Button type="submit" variant="primary" disabled={isSubmitting}>
              <ShieldIcon aria-hidden />
              {t('elevation.submit')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
