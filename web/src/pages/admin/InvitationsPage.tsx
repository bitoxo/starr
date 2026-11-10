import { zodResolver } from '@hookform/resolvers/zod'
import { InfoIcon, MailIcon, SendIcon } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import type { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import {
  defaultEntitlements,
  useCreateInvitation,
  useInvitations,
  useRevokeInvitation,
  type Entitlement,
  type Invitation,
  type Link as OneTimeLink,
} from '@/api/members'
import { PageHeader } from '@/components/AppShell'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import { type Column, DataTable } from '@/components/DataTable'
import { EmptyState } from '@/components/EmptyState'
import { FormField } from '@/components/FormField'
import {
  CategoryList,
  CopyLink,
  EntitlementsEditor,
  ErrorNotice,
  NoCategoriesWarning,
} from '@/components/members'
import { StatusChip, type StatusTone } from '@/components/StatusChip'
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
import { useElevation } from '@/lib/elevation'
import { formatDate, formatRelative } from '@/lib/format'
import { useUrlSort } from '@/lib/urlState'
import { createInvitationSchema } from '@/lib/validation'

import { PresetAccountsPicker, type PresetChoice } from './PresetAccountsPicker'

const STATE_TONE: Record<Invitation['state'], StatusTone> = {
  pending: 'info',
  accepted: 'ok',
  revoked: 'neutral',
  expired: 'warn',
}

/** What ended an invitation, or when it will expire (ADM-14: no "in 7 days" once revoked). */
function Ending({ invitation }: { invitation: Invitation }) {
  const { t } = useTranslation('admin')
  const at =
    invitation.state === 'accepted'
      ? invitation.usedAt
      : invitation.state === 'revoked'
        ? invitation.revokedAt
        : invitation.expiresAt
  if (!at) return <span className="text-text-3">–</span>
  if (invitation.state === 'pending') {
    return <span title={formatDate(at)}>{formatRelative(at)}</span>
  }
  return <span>{t(`invitations.ended.${invitation.state}`, { date: formatDate(at) })}</span>
}

export default function InvitationsPage() {
  const { t } = useTranslation('admin')
  const invitations = useInvitations()
  const revoke = useRevokeInvitation()
  const errorMessage = useApiErrorMessage()
  const [creating, setCreating] = useState(false)
  const [revoking, setRevoking] = useState<Invitation | null>(null)
  const [sort, setSort] = useUrlSort({ column: 'created', direction: 'desc' })

  const addButton = (
    <Button variant="primary" onClick={() => setCreating(true)}>
      <SendIcon aria-hidden />
      {t('invitations.add')}
    </Button>
  )
  const label = (i: Invitation) => i.presetUsername ?? i.presetEmail ?? t('invitations.anyUsername')

  const columns: Column<Invitation>[] = [
    {
      id: 'invitation',
      header: t('invitations.columns.invitation'),
      className: 'min-w-64',
      sortValue: label,
      cell: (i) => (
        <div className="flex flex-col leading-tight">
          <span className="text-text">{label(i)}</span>
          <span className="type-data text-sm text-text-3">
            {i.usedBy
              ? t('invitations.usedBy', { name: i.usedBy })
              : [
                  i.presetUsername ? i.presetEmail : null,
                  i.createdBy && t('invitations.by', { name: i.createdBy }),
                ]
                  .filter(Boolean)
                  .join(' · ')}
          </span>
        </div>
      ),
    },
    {
      id: 'state',
      header: t('invitations.columns.state'),
      className: 'w-32',
      sortValue: (i) => i.state,
      cell: (i) => (
        <StatusChip tone={STATE_TONE[i.state]}>{t(`invitations.states.${i.state}`)}</StatusChip>
      ),
    },
    {
      id: 'categories',
      header: t('invitations.columns.categories'),
      className: 'min-w-48',
      cell: (i) => (
        <>
          <CategoryList
            entitlements={i.presetEntitlements}
            empty={t('members.noCategories')}
            modes
          />
          {i.presetAccounts.length > 0 && (
            <div className="mt-1 text-xs text-text-3">
              {t('invitations.namesAccounts', {
                names: i.presetAccounts.map((a) => a.username ?? a.remoteId).join(', '),
              })}
            </div>
          )}
        </>
      ),
    },
    {
      id: 'created',
      header: t('invitations.columns.created'),
      className: 'w-36',
      sortValue: (i) => i.createdAt,
      cell: (i) => <span className="type-data text-sm text-text-2">{formatDate(i.createdAt)}</span>,
    },
    {
      id: 'expires',
      header: t('invitations.columns.expires'),
      className: 'w-44',
      sortValue: (i) => i.expiresAt,
      cell: (i) => (
        <span className="type-data text-sm text-text-2">
          <Ending invitation={i} />
        </span>
      ),
    },
    {
      id: 'actions',
      header: t('invitations.columns.actions'),
      align: 'end',
      className: 'w-28',
      cell: (i) =>
        i.state === 'pending' ? (
          <Button
            size="sm"
            variant="ghost"
            aria-label={t('invitations.revokeLabel', { name: label(i) })}
            onClick={() => setRevoking(i)}
          >
            {t('invitations.revoke')}
          </Button>
        ) : null,
    },
  ]

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('invitations.title')}
        description={t('invitations.description')}
        actions={invitations.data?.length ? addButton : undefined}
      />
      {invitations.data && invitations.data.length === 0 ? (
        <EmptyState
          icon={MailIcon}
          title={t('invitations.emptyTitle')}
          description={t('invitations.emptyDescription')}
          action={addButton}
        />
      ) : (
        <>
          <DataTable
            label={t('invitations.title')}
            columns={columns}
            rows={invitations.data}
            rowKey={(i) => i.id}
            sort={sort}
            onSortChange={setSort}
            loading={invitations.isPending}
            error={invitations.isError ? errorMessage(invitations.error) : null}
            onRetry={() => void invitations.refetch()}
          />
          <p className="mt-3 flex items-center gap-2 text-sm text-text-3">
            <InfoIcon aria-hidden className="size-4 shrink-0" />
            {t('invitations.linkOnce')}
          </p>
        </>
      )}
      {/* ADM-07: a revoked link cannot be revived. */}
      <ConfirmDialog
        open={revoking !== null}
        onOpenChange={(open) => !open && setRevoking(null)}
        title={t('invitations.revokeTitle', { name: revoking ? label(revoking) : '' })}
        description={t('invitations.revokeText')}
        confirmLabel={t('invitations.revoke')}
        pending={revoke.isPending}
        onConfirm={() =>
          revoking &&
          revoke.mutate(revoking.id, {
            onSuccess: () => {
              setRevoking(null)
              toast(t('invitations.revoked'))
            },
            onError: (error) => toast.error(errorMessage(error)),
          })
        }
      />
      <CreateInvitationDialog open={creating} onOpenChange={setCreating} />
    </>
  )
}

type InviteValues = z.infer<typeof createInvitationSchema>

function CreateInvitationDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useTranslation(['admin', 'common'])
  const create = useCreateInvitation()
  const elevation = useElevation()
  const errorMessage = useApiErrorMessage()
  const [entitlements, setEntitlements] = useState<Entitlement[]>(defaultEntitlements)
  const [link, setLink] = useState<OneTimeLink | null>(null)
  const [accounts, setAccounts] = useState<PresetChoice>({})
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors, isSubmitting },
  } = useForm<InviteValues>({
    resolver: zodResolver(createInvitationSchema),
    defaultValues: { expiresInDays: 7, username: '', email: '' },
  })

  const close = (next: boolean) => {
    onOpenChange(next)
    if (!next) {
      reset()
      create.reset()
      setLink(null)
      setEntitlements(defaultEntitlements())
      setAccounts({})
    }
  }

  const onSubmit = handleSubmit(async (values) => {
    // Naming existing accounts adopts them: the server asks for a fresh
    // elevation (M10 2) and the action runs again after it.
    const result = await elevation
      .run(() =>
        create.mutateAsync({
          expiresInDays: values.expiresInDays,
          username: values.username || undefined,
          email: values.email || undefined,
          entitlements,
          accounts: Object.entries(accounts).map(([serviceId, remoteId]) => ({
            serviceId: Number(serviceId),
            remoteId,
          })),
        }),
      )
      .catch(() => null)
    if (result) setLink(result.link)
  })

  return (
    <>
      <Dialog open={open} onOpenChange={close}>
        <DialogContent
          className="max-w-lg"
          // The link is shown once: a stray click outside does not close it (Esc and Done do).
          onInteractOutside={(e) => {
            if (link) e.preventDefault()
          }}
        >
          {link ? (
            <>
              <DialogHeader>
                <DialogTitle>{t('invitations.dialog.createdTitle')}</DialogTitle>
                <DialogDescription>
                  {t('invitations.dialog.createdDescription', { date: formatDate(link.expiresAt) })}
                </DialogDescription>
              </DialogHeader>
              <DialogBody>
                <CopyLink url={link.url} label={t('invitations.columns.invitation')} />
              </DialogBody>
              <DialogFooter>
                <Button onClick={() => close(false)}>{t('invitations.dialog.done')}</Button>
              </DialogFooter>
            </>
          ) : (
            <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
              <DialogHeader>
                <DialogTitle>{t('invitations.dialog.title')}</DialogTitle>
                <DialogDescription>{t('invitations.dialog.description')}</DialogDescription>
              </DialogHeader>
              <DialogBody>
                {create.isError && <ErrorNotice>{errorMessage(create.error)}</ErrorNotice>}
                <FormField
                  label={t('invitations.dialog.expires')}
                  error={errors.expiresInDays?.message}
                  type="number"
                  min={1}
                  max={30}
                  className="type-data w-28"
                  {...register('expiresInDays', { valueAsNumber: true })}
                />
                <FormField
                  label={t('invitations.dialog.username')}
                  hint={t('invitations.dialog.usernameHint')}
                  error={errors.username?.message}
                  autoCapitalize="none"
                  spellCheck={false}
                  autoComplete="off"
                  {...register('username')}
                />
                <FormField
                  label={t('invitations.dialog.email')}
                  hint={t('invitations.dialog.emailHint')}
                  error={errors.email?.message}
                  type="email"
                  autoComplete="off"
                  {...register('email')}
                />
                <fieldset className="flex flex-col gap-2">
                  <legend className="mb-1.5 text-sm font-medium text-text-2">
                    {t('invitations.dialog.entitlements')}
                  </legend>
                  <EntitlementsEditor
                    value={entitlements}
                    onChange={setEntitlements}
                    adultHint={t('memberDetail.adultHint')}
                  />
                </fieldset>
                <NoCategoriesWarning entitlements={entitlements} />
                <PresetAccountsPicker value={accounts} onChange={setAccounts} />
              </DialogBody>
              <DialogFooter>
                <Button type="button" variant="ghost" onClick={() => close(false)}>
                  {t('common:actions.cancel')}
                </Button>
                <Button type="submit" variant="primary" disabled={isSubmitting}>
                  {t('invitations.dialog.submit')}
                </Button>
              </DialogFooter>
            </form>
          )}
        </DialogContent>
      </Dialog>
      {elevation.dialog}
    </>
  )
}
