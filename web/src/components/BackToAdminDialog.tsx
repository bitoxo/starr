import { zodResolver } from '@hookform/resolvers/zod'
import { ShieldIcon } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import { elevate, switchAccount } from '@/api/security'
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
 * Member view → admin: re-authenticate (password, plus a code when the admin
 * has TOTP), then switch. Both calls rotate the session.
 */
export function BackToAdminDialog({
  open,
  onOpenChange,
  adminName,
  totpEnabled,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  adminName: string
  totpEnabled: boolean
}) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const errorMessage = useApiErrorMessage()
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

  const close = (next: boolean) => {
    onOpenChange(next)
    if (!next) reset()
  }

  const onSubmit = handleSubmit(async ({ password, code }) => {
    clearErrors('root')
    try {
      await elevate(totpEnabled ? { password, code } : { password })
      await switchAccount()
      close(false)
      navigate('/admin/inbox', { replace: true })
    } catch (error) {
      setError('root', { message: errorMessage(error) })
    }
  })

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
          <DialogHeader>
            <DialogTitle>{t('backToAdmin.title')}</DialogTitle>
            <DialogDescription>
              {t(totpEnabled ? 'backToAdmin.descriptionTotp' : 'backToAdmin.description', {
                name: adminName,
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogBody>
            {errors.root?.message && <ErrorNotice>{errors.root.message}</ErrorNotice>}
            <FormField
              label={t('backToAdmin.password')}
              error={errors.password?.message}
              type="password"
              autoComplete="current-password"
              autoFocus
              {...register('password')}
            />
            {totpEnabled && (
              <FormField
                label={t('backToAdmin.code')}
                error={errors.code?.message}
                autoComplete="one-time-code"
                className="type-data tracking-widest"
                {...register('code')}
              />
            )}
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => close(false)}>
              {t('actions.cancel')}
            </Button>
            <Button type="submit" variant="primary" disabled={isSubmitting}>
              <ShieldIcon aria-hidden />
              {t('backToAdmin.submit')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
