import {
  ArrowLeftIcon,
  KeyRoundIcon,
  PauseIcon,
  PlayIcon,
  RotateCcwIcon,
  Trash2Icon,
} from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

import { useMemberAccountPlan } from '@/api/accounts'
import { useApiErrorMessage } from '@/api/errors'
import { useMemberLibraries } from '@/api/libraries'
import {
  useDeleteMember,
  useMember,
  useMemberAction,
  useResetPassword,
  useSaveEntitlements,
  type Entitlement,
  type Link as OneTimeLink,
  type MemberDetail,
} from '@/api/members'
import { PageHeader } from '@/components/AppShell'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import { CopyLink, EntitlementsEditor, ErrorNotice, MemberStatusChip } from '@/components/members'
import { UnsavedChangesGuard } from '@/components/UnsavedChanges'
import { CategoryDot } from '@/components/CategoryDot'
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
import { useElevation } from '@/lib/elevation'
import { deviceLabel, formatDate, formatDateTime, formatRelative } from '@/lib/format'

import { AccountNameNotes, PlanStateChip, StepList } from './AccountPlan'

export default function MemberDetailPage() {
  const { t } = useTranslation('admin')
  const id = Number(useParams().id)
  const member = useMember(id)
  const errorMessage = useApiErrorMessage()

  return (
    <>
      <Link
        to="/admin/people/members"
        className="mb-4 inline-flex items-center gap-1.5 rounded-control text-sm text-text-2 hover:text-text"
      >
        <ArrowLeftIcon aria-hidden className="size-4" />
        {t('memberDetail.back')}
      </Link>
      {member.isError && <ErrorNotice>{errorMessage(member.error)}</ErrorNotice>}
      {member.data && <Detail member={member.data} />}
    </>
  )
}

function Section({
  title,
  description,
  children,
}: {
  title: string
  description?: string
  children: ReactNode
}) {
  return (
    <section className="flex flex-col gap-4 rounded-panel border border-line bg-night-1 p-5 sm:p-6">
      <div className="flex flex-col gap-1">
        <h2 className="type-display text-lg font-semibold text-text">{title}</h2>
        {description && <p className="text-sm text-text-2">{description}</p>}
      </div>
      {children}
    </section>
  )
}

function Detail({ member }: { member: MemberDetail }) {
  const { t } = useTranslation('admin')
  const retired = member.status === 'retired'

  return (
    <>
      <PageHeader
        variant="admin"
        title={member.displayName}
        description={t('memberDetail.created', { date: formatDate(member.createdAt) })}
        actions={<MemberStatusChip member={member} />}
      />
      {member.deletion && <PendingDeletion member={member} />}
      <div className="grid gap-5 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <div className="flex flex-col gap-5">
          <Section title={t('memberDetail.profile')}>
            <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-base">
              <dt className="text-text-3">{t('memberDetail.username')}</dt>
              <dd className="type-data text-text">{member.username}</dd>
              <dt className="text-text-3">{t('memberDetail.email')}</dt>
              <dd className="text-text">{member.email ?? t('memberDetail.noEmail')}</dd>
              <dt className="text-text-3">{t('memberDetail.displayName')}</dt>
              <dd className="text-text">{member.displayName}</dd>
            </dl>
          </Section>
          <EntitlementsSection
            key={JSON.stringify(member.entitlements)}
            member={member}
            disabled={retired}
          />
          {!retired && <LibrariesSection memberId={member.id} />}
          <AccountsSection memberId={member.id} username={member.username} />
        </div>
        <div className="flex flex-col gap-5">
          {!retired && <Actions member={member} />}
          <Section title={t('memberDetail.sessions')}>
            {member.sessions.length === 0 ? (
              <p className="text-sm text-text-3">{t('memberDetail.sessionsEmpty')}</p>
            ) : (
              <ul className="flex flex-col divide-y divide-line">
                {member.sessions.map((s) => (
                  <li key={s.id} className="flex flex-col py-2 first:pt-0 last:pb-0">
                    <span className="text-base text-text">
                      {deviceLabel(s.userAgent) ?? t('memberDetail.unknownDevice')}
                    </span>
                    <span className="type-data text-sm text-text-3">
                      {s.ip ?? '–'} ·{' '}
                      {t('memberDetail.lastSeen', { when: formatRelative(s.lastSeenAt) })}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </Section>
        </div>
      </div>
    </>
  )
}

function PendingDeletion({ member }: { member: MemberDetail }) {
  const { t } = useTranslation('admin')
  const action = useMemberAction(member.id)
  const errorMessage = useApiErrorMessage()
  if (!member.deletion) return null
  return (
    <div className="chip mb-5 flex flex-wrap items-center justify-between gap-4 rounded-panel px-5 py-4 [--chip:var(--color-bad)]">
      <div className="flex flex-col gap-0.5">
        <p className="font-semibold">{t('memberDetail.pendingTitle')}</p>
        <p className="text-sm">
          {t('memberDetail.pendingDescription', {
            by: member.deletion.requestedBy,
            at: formatDate(member.deletion.requestedAt),
            until: formatDateTime(member.deletion.graceUntil),
          })}
        </p>
      </div>
      <Button
        // A banner action, not the page's primary one: neutral (ADM-16).
        disabled={action.isPending}
        onClick={() =>
          action.mutate('restore', {
            // A restored member comes back suspended (domain model §1): say so and offer
            // reactivation right there (ADM-13).
            onSuccess: () =>
              toast(t('memberDetail.restored'), {
                duration: 10_000,
                action: {
                  label: t('memberDetail.reactivate'),
                  // Promise callbacks: the admin may have left the page meanwhile.
                  onClick: () =>
                    void action.mutateAsync('reactivate').then(
                      () => toast(t('memberDetail.reactivated')),
                      (error: unknown) => toast.error(errorMessage(error)),
                    ),
                },
              }),
            onError: (error) => toast.error(errorMessage(error)),
          })
        }
      >
        <RotateCcwIcon aria-hidden />
        {t('memberDetail.restore')}
      </Button>
    </div>
  )
}

const adultOn = (list: Entitlement[]) => list.some((e) => e.category === 'adult' && e.enabled)

/**
 * Entitlements (ADM-11, ADM-12): Save only with changes, a warning before leaving with unsaved
 * ones, a toast after saving. Switching NSFW on asks first, in the words of the operator
 * statement; the hint then says since when it is on.
 */
function EntitlementsSection({ member, disabled }: { member: MemberDetail; disabled: boolean }) {
  const { t } = useTranslation(['admin', 'common'])
  const [value, setValue] = useState<Entitlement[]>(member.entitlements)
  const save = useSaveEntitlements(member.id)
  const errorMessage = useApiErrorMessage()
  const dirty = JSON.stringify(value) !== JSON.stringify(member.entitlements)

  const adultHint =
    member.adultSince && adultOn(value)
      ? t('memberDetail.adultSince', { date: formatDate(member.adultSince) })
      : t('memberDetail.adultHint')

  return (
    <Section
      title={t('memberDetail.entitlements')}
      description={t('memberDetail.entitlementsDescription')}
    >
      <UnsavedChangesGuard when={dirty && !save.isPending} />
      <EntitlementsEditor
        value={value}
        onChange={setValue}
        adultHint={adultHint}
        disabled={disabled}
        memberName={member.displayName}
      />
      {!disabled && (
        <Button
          className="self-start"
          disabled={!dirty || save.isPending}
          onClick={() =>
            // Promise callbacks: the section remounts with the saved entitlements.
            save.mutateAsync(value).then(
              () => toast(t('memberDetail.entitlementsSaved')),
              (error: unknown) => toast.error(errorMessage(error)),
            )
          }
        >
          {t('memberDetail.saveEntitlements')}
        </Button>
      )}
    </Section>
  )
}

function ActionRow({ hint, children }: { hint: string; children: ReactNode }) {
  return (
    <div className="flex flex-col items-start gap-1.5">
      {children}
      <p className="text-sm text-text-3">{hint}</p>
    </div>
  )
}

function Actions({ member }: { member: MemberDetail }) {
  const { t } = useTranslation('admin')
  const action = useMemberAction(member.id)
  const reset = useResetPassword(member.id)
  const errorMessage = useApiErrorMessage()
  const [link, setLink] = useState<OneTimeLink | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [suspending, setSuspending] = useState(false)
  const onError = (error: unknown) => toast.error(errorMessage(error))

  return (
    <Section title={t('memberDetail.actions')}>
      {member.status === 'active' ? (
        <ActionRow hint={t('memberDetail.suspendHint')}>
          <Button disabled={action.isPending} onClick={() => setSuspending(true)}>
            <PauseIcon aria-hidden />
            {t('memberDetail.suspend')}
          </Button>
          {/* ADM-07: suspending ends every session at once. */}
          <ConfirmDialog
            open={suspending}
            onOpenChange={setSuspending}
            title={t('memberDetail.suspendTitle', { name: member.displayName })}
            description={t('memberDetail.suspendText')}
            confirmLabel={t('memberDetail.suspend')}
            pending={action.isPending}
            onConfirm={() =>
              action.mutate('suspend', {
                onSuccess: () => {
                  setSuspending(false)
                  toast(t('memberDetail.suspended'))
                },
                onError,
              })
            }
          />
        </ActionRow>
      ) : (
        <ActionRow hint={t('memberDetail.reactivateHint')}>
          <Button
            disabled={action.isPending}
            onClick={() =>
              action.mutate('reactivate', {
                onSuccess: () => toast(t('memberDetail.reactivated')),
                onError,
              })
            }
          >
            <PlayIcon aria-hidden />
            {t('memberDetail.reactivate')}
          </Button>
        </ActionRow>
      )}
      {member.status === 'active' && (
        <ActionRow hint={t('memberDetail.resetPasswordHint')}>
          <Button
            disabled={reset.isPending}
            onClick={() => reset.mutate(undefined, { onSuccess: setLink, onError })}
          >
            <KeyRoundIcon aria-hidden />
            {t('memberDetail.resetPassword')}
          </Button>
        </ActionRow>
      )}
      <ActionRow hint={t('memberDetail.deleteHint')}>
        <Button variant="ghost" className="text-bad" onClick={() => setDeleting(true)}>
          <Trash2Icon aria-hidden />
          {t('memberDetail.delete')}
        </Button>
      </ActionRow>

      <Dialog open={link !== null} onOpenChange={(open) => !open && setLink(null)}>
        <DialogContent
          className="max-w-lg"
          // Shown once: a stray click outside does not close it (Esc and Done do, ADM-37).
          onInteractOutside={(e) => e.preventDefault()}
        >
          <DialogHeader>
            <DialogTitle>{t('memberDetail.resetTitle')}</DialogTitle>
            <DialogDescription>
              {link &&
                t('memberDetail.resetDescription', {
                  name: member.displayName,
                  date: formatDate(link.expiresAt),
                })}
            </DialogDescription>
          </DialogHeader>
          <DialogBody>
            {link && <CopyLink url={link.url} label={t('memberDetail.resetTitle')} />}
          </DialogBody>
          <DialogFooter>
            <Button onClick={() => setLink(null)}>{t('members.addDialog.done')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <DeleteDialog member={member} open={deleting} onOpenChange={setDeleting} />
    </Section>
  )
}

function DeleteDialog({
  member,
  open,
  onOpenChange,
}: {
  member: MemberDetail
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useTranslation(['admin', 'common'])
  const remove = useDeleteMember(member.id)
  const accounts = useMemberAccountPlan(member.id)
  const errorMessage = useApiErrorMessage()
  const elevation = useElevation()
  const navigate = useNavigate()
  const [confirm, setConfirm] = useState('')
  const matches = confirm.trim().toLowerCase() === member.username
  // D16 preview: what the deletion order does in each media server.
  const owned = (accounts.data ?? []).filter((p) => p.cell.account)

  const submit = async () => {
    try {
      // Deleting a member who owns service accounts needs a fresh elevation.
      const result = await elevation.run(() => remove.mutateAsync(member.username))
      onOpenChange(false)
      if (result.outcome === 'deleted') {
        toast(t('memberDetail.deleted'))
        navigate('/admin/people/members', { replace: true })
      } else if (result.graceUntil) {
        toast(t('memberDetail.deletionScheduled', { date: formatDateTime(result.graceUntil) }))
      }
    } catch {
      // Cancelled or failed: the dialog shows the error and stays open.
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next)
        if (!next) {
          setConfirm('')
          remove.reset()
        }
      }}
    >
      <DialogContent>
        <form
          className="flex flex-col gap-5"
          onSubmit={(e) => {
            e.preventDefault()
            if (matches) void submit()
          }}
        >
          <DialogHeader>
            <DialogTitle>{t('memberDetail.deleteTitle', { name: member.displayName })}</DialogTitle>
            <DialogDescription>{t('memberDetail.deleteDescription')}</DialogDescription>
          </DialogHeader>
          <DialogBody>
            {remove.isError && <ErrorNotice>{errorMessage(remove.error)}</ErrorNotice>}
            {owned.length > 0 && (
              <div className="flex flex-col gap-1.5">
                <span className="text-sm font-semibold text-text">
                  {t('memberDetail.deletePreview')}
                </span>
                <span className="text-xs text-text-3">{t('memberDetail.deletePreviewHint')}</span>
                <ul className="flex flex-col gap-1 text-sm">
                  {owned.map((p) => {
                    const name = p.cell.account?.username ?? p.cell.linkedUsername ?? ''
                    // D16 (maintainer 2026-11-03): only accounts Starr created
                    // are deleted; adopted ones are disabled and released.
                    const key =
                      p.cell.state === 'ignored' || p.cell.state === 'privileged'
                        ? 'memberDetail.deleteAccountStays'
                        : p.cell.origin === 'provisioned'
                          ? 'memberDetail.deleteAccountGoes'
                          : 'memberDetail.deleteAccountReleased'
                    const goes = key === 'memberDetail.deleteAccountGoes'
                    return (
                      <li key={p.serviceId} className={goes ? 'text-text' : 'text-text-2'}>
                        {t(key, { service: p.serviceName, name })}
                      </li>
                    )
                  })}
                </ul>
              </div>
            )}
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="delete-confirm">
                {t('memberDetail.deleteConfirm', { name: member.username })}
              </Label>
              <Input
                id="delete-confirm"
                value={confirm}
                autoComplete="off"
                autoCapitalize="none"
                spellCheck={false}
                onChange={(e) => setConfirm(e.target.value)}
              />
            </div>
          </DialogBody>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {t('common:actions.cancel')}
            </Button>
            <Button
              type="submit"
              className="border-bad text-bad"
              disabled={!matches || remove.isPending}
            >
              <Trash2Icon aria-hidden />
              {t('memberDetail.deleteSubmit')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
      {elevation.dialog}
    </Dialog>
  )
}

/** What the member would get per media server (spec 0005 M4; read-only). */
function LibrariesSection({ memberId }: { memberId: number }) {
  const { t } = useTranslation(['admin', 'common'])
  const libraries = useMemberLibraries(memberId)
  const errorMessage = useApiErrorMessage()
  const list = libraries.data ?? []
  return (
    <Section
      title={t('memberDetail.libraries.title')}
      description={t('memberDetail.libraries.description')}
    >
      {libraries.isError && <ErrorNotice>{errorMessage(libraries.error)}</ErrorNotice>}
      {libraries.isSuccess && list.length === 0 && (
        <p className="text-sm text-text-3">{t('memberDetail.libraries.empty')}</p>
      )}
      {list.length > 0 && (
        <ul className="flex flex-col divide-y divide-line">
          {list.map((s) => (
            <li key={s.serviceId} className="flex flex-col gap-1.5 py-3 first:pt-0 last:pb-0">
              <span className="font-medium text-text">{s.serviceName}</span>
              {s.libraries.length === 0 ? (
                <span className="text-sm text-text-3">{t('memberDetail.libraries.none')}</span>
              ) : (
                <ul className="flex flex-wrap gap-1.5">
                  {s.libraries.map((l) => (
                    <li
                      key={l.id}
                      className="inline-flex items-center gap-1.5 rounded-full border border-line bg-night-0 px-2.5 py-0.5 text-sm text-text"
                    >
                      <CategoryDot category={l.category} />
                      {l.name ?? t('memberDetail.libraries.gone')}
                    </li>
                  ))}
                </ul>
              )}
              {s.unmappedCategories.length > 0 && (
                <span className="text-sm text-warn">
                  {t('memberDetail.libraries.unmapped', {
                    categories: s.unmappedCategories
                      .map((c) => t(`common:categories.${c}`))
                      .join(', '),
                  })}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </Section>
  )
}

/** The member's account per media server: decision, planned state and steps (plan only). */
function AccountsSection({ memberId, username }: { memberId: number; username: string }) {
  const { t } = useTranslation('admin')
  const plan = useMemberAccountPlan(memberId)
  if (!plan.data || plan.data.length === 0) return null
  return (
    <Section title={t('memberDetail.accounts')} description={t('memberDetail.accountsHint')}>
      <ul className="flex flex-col divide-y divide-line">
        {plan.data.map((p) => {
          return (
            <li key={p.serviceId} className="flex flex-col gap-2 py-3 first:pt-0 last:pb-0">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Link
                  to={`/admin/media/accounts?service=${p.serviceId}`}
                  className="font-medium text-text hover:underline"
                >
                  {p.serviceName}
                </Link>
                {p.cell.state ? (
                  <PlanStateChip state={p.cell.state} />
                ) : (
                  <span className="text-sm text-text-2">
                    {t(`accounts.decision.${p.cell.decision}`)}
                  </span>
                )}
              </div>
              <AccountNameNotes cell={p.cell} username={username} className="text-sm text-text-2" />
              {p.cell.password === 'deferred' && (
                <span className="text-sm text-text-3">{t('accounts.matrix.oldPassword')}</span>
              )}
              <StepList steps={p.cell.steps} />
            </li>
          )
        })}
      </ul>
      <Link
        to="/admin/media/accounts"
        className="text-sm text-text-2 hover:text-text hover:underline"
      >
        {t('memberDetail.accountsDecide')}
      </Link>
    </Section>
  )
}
