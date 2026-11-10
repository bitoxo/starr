import { zodResolver } from '@hookform/resolvers/zod'
import { ShieldAlertIcon, Trash2Icon, UserPlusIcon } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import type { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import type { Link as OneTimeLink } from '@/api/members'
import {
  useAdminAction,
  useAdmins,
  useCreateAdmin,
  useRemoveAdmin,
  type Admin,
} from '@/api/security'
import { PageHeader } from '@/components/AppShell'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import { type Column, DataTable } from '@/components/DataTable'
import { FormField } from '@/components/FormField'
import { CopyLink, ErrorNotice } from '@/components/members'
import { StatusChip } from '@/components/StatusChip'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useEnsureElevated } from '@/lib/elevation'
import { formatDate, formatDateTime } from '@/lib/format'
import { useUrlSort } from '@/lib/urlState'
import { createAdminSchema } from '@/lib/validation'

export default function AdminsPage() {
  const { t } = useTranslation(['admin', 'common'])
  const admins = useAdmins()
  const action = useAdminAction()
  const errorMessage = useApiErrorMessage()
  const [adding, setAdding] = useState(false)
  const [removing, setRemoving] = useState<Admin | null>(null)
  const [suspending, setSuspending] = useState<Admin | null>(null)
  const [sort, setSort] = useUrlSort()
  // Adding, suspending, reactivating and removing admins needs a fresh re-authentication: asked before
  // the action's dialog opens (ADM-34).
  const elevated = useEnsureElevated()
  const onError = (error: unknown) => toast.error(errorMessage(error))

  const columns: Column<Admin>[] = [
    {
      id: 'admin',
      header: t('admins.columns.admin'),
      className: 'min-w-56',
      sortValue: (a) => a.displayName,
      cell: (admin) => (
        <span className="flex flex-col leading-tight">
          <span className="font-medium text-text">
            {admin.displayName}
            {admin.current && (
              <span className="type-data ml-2 rounded-full border border-line px-2 py-0.5 text-xs text-text-2">
                {t('admins.you')}
              </span>
            )}
          </span>
          <span className="type-data text-sm text-text-3">{admin.username}</span>
        </span>
      ),
    },
    {
      id: 'status',
      header: t('admins.columns.status'),
      className: 'w-32',
      cell: (admin) => (
        <StatusChip tone={admin.status === 'active' ? 'ok' : 'warn'}>
          {t(`admins.status.${admin.status}`)}
        </StatusChip>
      ),
    },
    {
      id: 'totp',
      header: t('admins.columns.totp'),
      className: 'w-28',
      cell: (admin) => (
        <StatusChip tone={admin.totpEnabled ? 'ok' : 'neutral'}>
          {admin.totpEnabled ? t('admins.totpOn') : t('admins.totpOff')}
        </StatusChip>
      ),
    },
    {
      id: 'created',
      header: t('admins.columns.created'),
      className: 'w-40',
      sortValue: (a) => a.createdAt,
      cell: (admin) => (
        <span className="type-data text-sm text-text-3">
          {formatDate(admin.createdAt)}
          {admin.createdBy && (
            <span className="block">{t('admins.createdBy', { name: admin.createdBy })}</span>
          )}
        </span>
      ),
    },
    {
      id: 'actions',
      header: t('admins.columns.actions'),
      align: 'end',
      className: 'w-52',
      cell: (admin) =>
        admin.current ? null : (
          <div className="flex justify-end gap-1">
            <Button
              size="sm"
              disabled={action.isPending}
              onClick={() =>
                admin.status === 'active'
                  ? elevated.ensure(() => setSuspending(admin))
                  : elevated.ensure(() =>
                      action.mutate(
                        { id: admin.id, action: 'reactivate' },
                        { onSuccess: () => toast(t('admins.reactivated')), onError },
                      ),
                    )
              }
            >
              {admin.status === 'active' ? t('admins.suspend') : t('admins.reactivate')}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="text-bad"
              onClick={() => elevated.ensure(() => setRemoving(admin))}
            >
              {t('admins.remove')}
            </Button>
          </div>
        ),
    },
  ]

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('admins.title')}
        description={t('admins.description')}
        actions={
          <Button variant="primary" onClick={() => elevated.ensure(() => setAdding(true))}>
            <UserPlusIcon aria-hidden />
            {t('admins.add')}
          </Button>
        }
      />
      <DataTable
        label={t('admins.title')}
        columns={columns}
        rows={admins.data}
        rowKey={(a) => a.id}
        sort={sort}
        onSortChange={setSort}
        loading={admins.isPending}
        error={admins.isError ? errorMessage(admins.error) : null}
        onRetry={() => void admins.refetch()}
      />
      {/* ADM-07: suspending ends the admin's sessions at once. */}
      <ConfirmDialog
        open={suspending !== null}
        onOpenChange={(open) => !open && setSuspending(null)}
        title={t('admins.suspendTitle', { name: suspending?.displayName ?? '' })}
        description={t('admins.suspendText')}
        confirmLabel={t('admins.suspend')}
        pending={action.isPending}
        onConfirm={() =>
          suspending &&
          action.mutate(
            { id: suspending.id, action: 'suspend' },
            {
              onSuccess: () => {
                setSuspending(null)
                toast(t('admins.suspended'))
              },
              onError,
            },
          )
        }
      />
      <AddAdminDialog open={adding} onOpenChange={setAdding} />
      {removing && <RemoveAdminDialog admin={removing} onClose={() => setRemoving(null)} />}
      {elevated.dialog}
    </>
  )
}

type AddValues = z.infer<typeof createAdminSchema>

export function AddAdminDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useTranslation(['admin', 'auth', 'common'])
  const create = useCreateAdmin()
  const errorMessage = useApiErrorMessage()
  const [created, setCreated] = useState<{ name: string; link: OneTimeLink } | null>(null)
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors },
  } = useForm<AddValues>({
    resolver: zodResolver(createAdminSchema),
    defaultValues: { username: '', displayName: '' },
  })

  const close = (next: boolean) => {
    onOpenChange(next)
    if (!next) {
      reset()
      create.reset()
      setCreated(null)
    }
  }

  const onSubmit = handleSubmit(async (values) => {
    const result = await create
      .mutateAsync({ username: values.username, displayName: values.displayName || undefined })
      .catch(() => null)
    if (result) setCreated({ name: result.admin.displayName, link: result.setPasswordLink })
  })

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent
        className="max-w-lg"
        // The link is shown once: a stray click outside does not close it (Esc and Done do).
        onInteractOutside={(e) => {
          if (created) e.preventDefault()
        }}
      >
        {created ? (
          <>
            <DialogHeader>
              <DialogTitle>{t('admins.addDialog.createdTitle')}</DialogTitle>
              <DialogDescription>
                {t('admins.addDialog.createdDescription', {
                  name: created.name,
                  date: formatDateTime(created.link.expiresAt),
                })}
              </DialogDescription>
            </DialogHeader>
            <DialogBody>
              <CopyLink url={created.link.url} label={t('admins.addDialog.link')} />
            </DialogBody>
            <DialogFooter>
              <Button onClick={() => close(false)}>{t('admins.addDialog.done')}</Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
            <DialogHeader>
              <DialogTitle>{t('admins.addDialog.title')}</DialogTitle>
              <DialogDescription>{t('admins.addDialog.description')}</DialogDescription>
            </DialogHeader>
            <DialogBody>
              {create.isError && <ErrorNotice>{errorMessage(create.error)}</ErrorNotice>}
              <FormField
                label={t('auth:fields.username')}
                hint={t('members.addDialog.usernameHint')}
                error={errors.username?.message}
                autoCapitalize="none"
                spellCheck={false}
                autoComplete="off"
                {...register('username')}
              />
              <FormField
                label={t('auth:fields.displayName')}
                hint={t('members.addDialog.displayNameHint')}
                error={errors.displayName?.message}
                autoComplete="off"
                {...register('displayName')}
              />
              {/* ADM-06: adding an admin without two-step verification pauses account writes. */}
              <p
                role="note"
                className="flex items-start gap-2.5 rounded-card border border-line bg-night-1 px-3 py-2.5 text-sm text-text-2"
              >
                <ShieldAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-warn" />
                {t('admins.addDialog.totpWarning')}
              </p>
            </DialogBody>
            <DialogFooter>
              <Button type="button" variant="ghost" onClick={() => close(false)}>
                {t('common:actions.cancel')}
              </Button>
              <Button type="submit" variant="primary" disabled={create.isPending}>
                {t('admins.addDialog.submit')}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}

function RemoveAdminDialog({ admin, onClose }: { admin: Admin; onClose: () => void }) {
  const { t } = useTranslation(['admin', 'common'])
  const remove = useRemoveAdmin()
  const errorMessage = useApiErrorMessage()
  const [confirm, setConfirm] = useState('')
  const matches = confirm.trim().toLowerCase() === admin.username

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form
          className="flex flex-col gap-5"
          onSubmit={(e) => {
            e.preventDefault()
            if (!matches) return
            remove.mutate(
              { id: admin.id, username: admin.username },
              {
                onSuccess: () => {
                  toast(t('admins.removed'))
                  onClose()
                },
              },
            )
          }}
        >
          <DialogHeader>
            <DialogTitle>{t('admins.removeTitle', { name: admin.displayName })}</DialogTitle>
            <DialogDescription>{t('admins.removeDescription')}</DialogDescription>
          </DialogHeader>
          <DialogBody>
            {remove.isError && <ErrorNotice>{errorMessage(remove.error)}</ErrorNotice>}
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="remove-admin-confirm">
                {t('admins.removeConfirm', { name: admin.username })}
              </Label>
              <Input
                id="remove-admin-confirm"
                value={confirm}
                autoComplete="off"
                autoCapitalize="none"
                spellCheck={false}
                onChange={(e) => setConfirm(e.target.value)}
              />
            </div>
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              {t('common:actions.cancel')}
            </Button>
            <Button
              type="submit"
              className="border-bad text-bad"
              disabled={!matches || remove.isPending}
            >
              <Trash2Icon aria-hidden />
              {t('admins.remove')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
