import {
  CheckCheckIcon,
  ChevronDownIcon,
  LinkIcon,
  PlusIcon,
  BanIcon,
  ClockIcon,
  ShieldCheckIcon,
} from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { toast } from 'sonner'

import {
  type AccountCell,
  type AccountDecision,
  type MatrixMember,
  type MatrixService,
  useAccountMatrix,
  useAccounts,
  useConfirmSuggestions,
  useDecideAccount,
} from '@/api/accounts'
import { useApiErrorMessage } from '@/api/errors'
import { ElevationCancelled, isElevated, useElevation, useEnsureElevated } from '@/lib/elevation'
import { compareText, formatTime } from '@/lib/format'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'

import { AccountNameNotes, CopyId, PlanStateChip } from './AccountPlan'

/** Members × media servers: decisions, suggestions and planned states (M3). */
export function AccountMatrixView() {
  const { t } = useTranslation(['admin', 'common'])
  const matrix = useAccountMatrix()
  if (matrix.isPending) return <p className="text-text-3">{t('common:status.loading')}</p>
  if (matrix.isError) {
    return (
      <p role="alert" className="text-bad">
        {t('accounts.errors.unavailable')}
      </p>
    )
  }
  const { members } = matrix.data
  // Columns in the order of the server tabs above (by name), each with its cell index (ADM-27).
  const services = matrix.data.services
    .map((service, index) => ({ ...service, index }))
    .sort((a, b) => compareText(a.name, b.name))
  const noOffSwitch = services.filter((s) => s.disablesByPassword).map((s) => s.name)
  return (
    <section aria-labelledby="matrix-title">
      <h2 id="matrix-title" className="type-display mb-1 text-lg font-semibold text-text">
        {t('accounts.matrix.title')}
      </h2>
      <p className="mb-4 max-w-2xl text-sm text-text-2">{t('accounts.matrix.hint')}</p>
      {noOffSwitch.length > 0 && (
        <p className="mb-4 max-w-2xl text-sm text-text-2">
          {t('accounts.matrix.disablesByPassword', { servers: noOffSwitch.join(', ') })}
        </p>
      )}
      {members.length === 0 ? (
        <p className="text-text-3">{t('accounts.matrix.noMembers')}</p>
      ) : (
        <>
          {/* Phones: one card per member, one line per server. */}
          <ul className="flex flex-col gap-3 md:hidden">
            {members.map((m) => (
              <li key={m.id} className="rounded-panel border border-line bg-night-1 px-4 py-3">
                <div className="mb-2 flex items-center justify-between gap-3">
                  <MemberName member={m} />
                  <ConfirmButton member={m} />
                </div>
                <dl className="flex flex-col gap-3">
                  {services.map((s) => (
                    <div key={s.id}>
                      <dt className="type-data mb-1 text-xs tracking-wide text-text-3 uppercase">
                        {s.name}
                      </dt>
                      <dd>
                        <Cell member={m} service={s} cell={m.cells[s.index]} />
                      </dd>
                    </div>
                  ))}
                </dl>
              </li>
            ))}
          </ul>
          {/* Inner scroll with the member column kept in view, never a wider page (ADM-27). */}
          <div className="hidden max-w-full min-w-0 overflow-x-auto rounded-panel border border-line bg-night-1 md:block">
            <table className="w-full border-separate border-spacing-0 text-left text-base">
              <thead>
                <tr className="type-data text-xs tracking-wide text-text-3 uppercase [&>th]:border-b [&>th]:border-line">
                  <th className="sticky left-0 z-[1] bg-night-1 px-4 py-3 font-medium">
                    {t('accounts.matrix.member')}
                  </th>
                  {services.map((s) => (
                    <th key={s.id} className="px-4 py-3 font-medium">
                      {s.name}
                      {s.disablesByPassword && (
                        <span className="block text-text-3 normal-case">
                          {t('accounts.matrix.noOffSwitch')}
                        </span>
                      )}
                    </th>
                  ))}
                  <th className="px-4 py-3 text-right font-medium">
                    <span className="sr-only">{t('accounts.columns.actions')}</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {members.map((m) => (
                  <tr
                    key={m.id}
                    className="align-middle [&:not(:last-child)>td]:border-b [&>td]:border-line"
                  >
                    <td className="sticky left-0 z-[1] min-w-44 border-r border-line bg-night-1 px-4 py-3">
                      <MemberName member={m} />
                    </td>
                    {services.map((s) => (
                      <td key={s.id} className="min-w-56 px-4 py-3">
                        <Cell member={m} service={s} cell={m.cells[s.index]} />
                      </td>
                    ))}
                    <td className="px-2 py-2">
                      <div className="flex justify-end">
                        <ConfirmButton member={m} />
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </section>
  )
}

function MemberName({ member }: { member: MatrixMember }) {
  const { t } = useTranslation()
  return (
    <div className="min-w-0">
      <Link
        to={`/admin/people/members/${member.id}`}
        className="font-semibold text-text hover:underline"
      >
        {member.displayName}
      </Link>
      <div className="type-data text-xs text-text-3">
        {member.username}
        {member.status !== 'active' && <> · {t(`memberStatus.${member.status}`)}</>}
      </div>
    </div>
  )
}

function ConfirmButton({ member }: { member: MatrixMember }) {
  const { t } = useTranslation('admin')
  const confirm = useConfirmSuggestions()
  const message = useApiErrorMessage()
  const elevation = useElevation()
  if (!member.cells.some((c) => c.suggestion)) return null
  const run = async () => {
    try {
      // Linking adopts accounts: the admin confirms it's them (M10 2).
      await elevation.run(() => confirm.mutateAsync(member.id))
      toast(t('accounts.matrix.confirmed', { name: member.username }))
    } catch (err) {
      if (err instanceof ElevationCancelled) return
      toast.error(message(err))
    }
  }
  return (
    <>
      <Button
        variant="secondary"
        size="sm"
        onClick={run}
        disabled={confirm.isPending}
        aria-label={t('accounts.matrix.confirmFor', { name: member.username })}
      >
        <CheckCheckIcon aria-hidden />
        {t('accounts.matrix.confirm')}
      </Button>
      {elevation.dialog}
    </>
  )
}

function Cell({
  member,
  service,
  cell,
}: {
  member: MatrixMember
  service: MatrixService
  cell: AccountCell | undefined
}) {
  const { t } = useTranslation('admin')
  if (!cell) return null
  return (
    <div className="flex flex-col items-start gap-1.5">
      <div className="flex flex-wrap items-center gap-1.5">
        {cell.state ? (
          <PlanStateChip state={cell.state} />
        ) : (
          <span className="text-sm text-text-2">{t(`accounts.decision.${cell.decision}`)}</span>
        )}
        <DecisionMenu member={member} service={service} cell={cell} />
      </div>
      <AccountNameNotes cell={cell} username={member.username} className="text-xs text-text-2" />
      {cell.lockedByStarr && (
        <span className="text-xs text-text-2">{t('accounts.matrix.lockedByStarr')}</span>
      )}
      {/* A locked account's old password no longer works. */}
      {cell.password === 'deferred' && !cell.lockedByStarr && (
        <span className="text-xs text-text-3">{t('accounts.matrix.oldPassword')}</span>
      )}
      {cell.password === 'pending' && (
        <span className="text-xs text-text-3">{t('accounts.matrix.switching')}</span>
      )}
      {cell.suggestion && (
        <span className="text-xs text-text-2">
          {t('accounts.matrix.suggested', {
            name: cell.suggestion.username ?? cell.suggestion.remoteId,
          })}
        </span>
      )}
      {cell.conflictAccount && (
        <span className="text-xs text-text-3">
          {t('accounts.matrix.nameTakenBy', {
            name: cell.conflictAccount.username ?? cell.conflictAccount.remoteId,
          })}
        </span>
      )}
      {cell.steps.length > 0 && (
        <span className="type-data text-xs text-text-3">
          {t('accounts.matrix.steps', { count: cell.steps.length })}
        </span>
      )}
    </div>
  )
}

function DecisionMenu({
  member,
  service,
  cell,
}: {
  member: MatrixMember
  service: MatrixService
  cell: AccountCell
}) {
  const { t } = useTranslation('admin')
  const decide = useDecideAccount()
  const message = useApiErrorMessage()
  const elevation = useElevation()
  // Linking adopts an account: re-authenticate before the dialog, not on top of it (ADM-34).
  const beforeLink = useEnsureElevated()
  const [linking, setLinking] = useState(false)
  const names = { member: member.username, server: service.name }

  const run = async (decision: AccountDecision, remoteId?: string) => {
    try {
      // Linking adopts an account: the server asks for a fresh elevation.
      await elevation.run(() =>
        decide.mutateAsync({ memberId: member.id, serviceId: service.id, decision, remoteId }),
      )
      toast(t('accounts.matrix.decided', names))
      setLinking(false)
    } catch (err) {
      if (err instanceof ElevationCancelled) return
      toast.error(message(err))
    }
  }
  const suggestion = cell.suggestion
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            disabled={decide.isPending}
            aria-label={t('accounts.matrix.decideFor', names)}
          >
            <ChevronDownIcon aria-hidden />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start">
          {suggestion && (
            <DropdownMenuItem onSelect={() => void run('linked', suggestion.remoteId)}>
              <LinkIcon aria-hidden />
              {t('accounts.matrix.linkSuggestion', {
                name: suggestion.username ?? suggestion.remoteId,
              })}
            </DropdownMenuItem>
          )}
          <DropdownMenuItem onSelect={() => beforeLink.ensure(() => setLinking(true))}>
            <LinkIcon aria-hidden />
            {t('accounts.matrix.linkOther')}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            disabled={cell.decision === 'create'}
            onSelect={() => void run('create')}
          >
            <PlusIcon aria-hidden />
            {t('accounts.matrix.create')}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={cell.decision === 'none'} onSelect={() => void run('none')}>
            <BanIcon aria-hidden />
            {t('accounts.matrix.none')}
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={cell.decision === 'undecided'}
            onSelect={() => void run('undecided')}
          >
            <ClockIcon aria-hidden />
            {t('accounts.matrix.undecided')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <LinkDialog
        open={linking}
        onOpenChange={setLinking}
        service={service}
        member={member}
        onLink={(remoteId) => void run('linked', remoteId)}
        busy={decide.isPending}
        elevatedUntil={beforeLink.elevatedUntil}
      />
      {elevation.dialog}
      {beforeLink.dialog}
    </>
  )
}

/** Pick any linkable account of the server (names may differ, D5). */
function LinkDialog({
  open,
  onOpenChange,
  service,
  member,
  onLink,
  busy,
  elevatedUntil,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  service: MatrixService
  member: MatrixMember
  onLink: (remoteId: string) => void
  busy: boolean
  elevatedUntil: string | null
}) {
  const { t } = useTranslation('admin')
  const accounts = useAccounts(open ? service.id : null)
  const [query, setQuery] = useState('')
  const linkable = (accounts.data?.accounts ?? []).filter(
    (a) =>
      !a.privileged &&
      a.ignoredAt == null &&
      a.goneSince == null &&
      a.linkedMember == null &&
      a.username.toLowerCase().includes(query.trim().toLowerCase()),
  )
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('accounts.link.title', { server: service.name })}</DialogTitle>
          <DialogDescription>
            {t('accounts.link.description', { member: member.username })}
          </DialogDescription>
          {isElevated(elevatedUntil) && elevatedUntil && (
            <p className="flex items-center gap-1.5 text-sm text-text-3">
              <ShieldCheckIcon aria-hidden className="size-4 text-ok" />
              {t('accounts.link.confirmedUntil', { time: formatTime(elevatedUntil) })}
            </p>
          )}
        </DialogHeader>
        <DialogBody>
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('accounts.link.search')}
            aria-label={t('accounts.link.search')}
          />
          {accounts.isSuccess && linkable.length === 0 ? (
            <p className="text-sm text-text-3">{t('accounts.link.empty')}</p>
          ) : (
            <ul className="flex flex-col divide-y divide-line">
              {linkable.map((a) => (
                <li key={a.remoteId} className="flex items-center justify-between gap-3 py-2">
                  <div className="flex min-w-0 items-center gap-1">
                    <span className="font-medium text-text" title={a.remoteId}>
                      {a.username}
                    </span>
                    <CopyId id={a.remoteId} name={a.username} />
                  </div>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={busy}
                    onClick={() => onLink(a.remoteId)}
                    aria-label={t('accounts.link.chooseFor', { name: a.username })}
                  >
                    {t('accounts.link.choose')}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </DialogBody>
      </DialogContent>
    </Dialog>
  )
}
