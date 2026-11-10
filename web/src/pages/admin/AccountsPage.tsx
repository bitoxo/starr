import { ContactIcon, EyeIcon, EyeOffIcon, LinkIcon, RefreshCwIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { toast } from 'sonner'

import {
  type AccountInventory,
  type ForeignAccount,
  useAccounts,
  useScanAccounts,
  useSetAccountIgnored,
} from '@/api/accounts'
import { useJobRun } from '@/api/runs'
import { useCategories } from '@/api/categories'
import { type Service, useServiceTypes, useServices } from '@/api/services'
import { PageHeader } from '@/components/AppShell'
import { EmptyState } from '@/components/EmptyState'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { accountChips } from '@/lib/accounts'
import { formatDateTime, formatRelative, compareText } from '@/lib/format'
import { serviceErrorKey } from '@/lib/services'
import { cn } from '@/lib/utils'

import { AccountMatrixView } from './AccountMatrix'
import { AccountPlanPanel, CopyId } from './AccountPlan'

export default function AccountsPage() {
  const { t } = useTranslation(['admin', 'common'])
  const services = useServices()
  const types = useServiceTypes()
  const categories = useCategories()
  const [params, setParams] = useSearchParams()
  const scan = useScanAccounts()
  const [scanJob, setScanJob] = useState<number | null>(null)

  // Starr scans the servers a category watches in (consume role).
  const consumed = new Set(
    (categories.data ?? []).flatMap((c) => c.consumers.map((r) => r.serviceId)),
  )
  // By name, the same order as the matrix columns (ADM-27).
  const withAccounts = (services.data ?? [])
    .filter(
      (s) =>
        consumed.has(s.id) &&
        types.data?.find((ty) => ty.typeId === s.adapterType)?.capabilities.includes('accounts'),
    )
    .sort((a, b) => compareText(a.name, b.name))
  // Default view: the members × servers matrix; `?service=` opens one server.
  const wanted = Number(params.get('service'))
  const selected = withAccounts.find((s) => s.id === wanted) ?? null
  const accounts = useAccounts(selected?.id ?? null)

  const startScan = async () => {
    try {
      setScanJob(await scan.mutateAsync())
      toast(t('accounts.scanQueued'))
    } catch {
      toast.error(t('accounts.errors.actionFailed'))
    }
  }

  const loading = services.isPending || types.isPending || categories.isPending

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('accounts.title')}
        description={t('accounts.description')}
        actions={
          withAccounts.length > 0 ? (
            // Neutral: "Go live" is this view's one gold action (ADM-16).
            <Button onClick={startScan} disabled={scanJob !== null}>
              <RefreshCwIcon
                aria-hidden
                className={cn(scanJob !== null && 'motion-safe:animate-spin')}
              />
              {scanJob !== null ? t('accounts.scanning') : t('accounts.scan')}
            </Button>
          ) : undefined
        }
      />
      {scanJob !== null && (
        <ScanWatcher
          jobId={scanJob}
          onDone={() => {
            setScanJob(null)
            void accounts.refetch()
          }}
        />
      )}

      {loading && <p className="text-text-3">{t('common:status.loading')}</p>}
      {(services.isError || categories.isError) && (
        <p role="alert" className="text-bad">
          {t('accounts.errors.unavailable')}
        </p>
      )}
      {!loading && services.isSuccess && withAccounts.length === 0 && (
        <EmptyState
          icon={ContactIcon}
          title={t('accounts.emptyTitle')}
          description={t('accounts.emptyDescription')}
          action={
            <Button asChild variant="secondary">
              <Link to="/admin/media/services">{t('accounts.toServices')}</Link>
            </Button>
          }
        />
      )}

      {withAccounts.length > 0 && (
        <ServerTabs
          services={withAccounts}
          selected={selected?.id ?? null}
          onSelect={(id) =>
            setParams(id === null ? {} : { service: String(id) }, { replace: true })
          }
        />
      )}
      {withAccounts.length > 0 && !selected && <AccountMatrixView />}
      {selected && (
        <>
          <AccountPlanPanel serviceId={selected.id} serviceName={selected.name} />
          <h2 className="type-display mb-3 text-lg font-semibold text-text">
            {t('accounts.inventoryTitle', { name: selected.name })}
          </h2>
          {accounts.isPending && <p className="text-text-3">{t('common:status.loading')}</p>}
          {accounts.isError && (
            <p role="alert" className="text-bad">
              {t('accounts.errors.unavailable')}
            </p>
          )}
          {accounts.data && <Inventory service={selected} inventory={accounts.data} />}
        </>
      )}
    </>
  )
}

/** Polls the queued scan until it finished. */
function ScanWatcher({ jobId, onDone }: { jobId: number; onDone: () => void }) {
  const run = useJobRun(jobId).data
  const finished = run != null && run.state !== 'running'
  useEffect(() => {
    if (finished) onDone()
    // onDone changes every render; the transition to finished is what matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finished])
  return null
}

function ServerTabs({
  services,
  selected,
  onSelect,
}: {
  services: Service[]
  selected: number | null
  onSelect: (id: number | null) => void
}) {
  const { t } = useTranslation('admin')
  return (
    <div
      role="tablist"
      aria-label={t('accounts.views')}
      className="mb-4 inline-flex max-w-full flex-wrap rounded-full border border-line bg-night-1 p-0.5"
    >
      <button
        type="button"
        role="tab"
        aria-selected={selected === null}
        onClick={() => onSelect(null)}
        className={cn(
          'cursor-pointer rounded-full px-3.5 py-1 text-sm text-text-2 transition-colors hover:text-text max-lg:min-h-11',
          selected === null && 'bg-night-2 text-text',
        )}
      >
        {t('accounts.membersTab')}
      </button>
      {services.map((s) => (
        <button
          key={s.id}
          type="button"
          role="tab"
          aria-selected={s.id === selected}
          onClick={() => onSelect(s.id)}
          className={cn(
            'cursor-pointer rounded-full px-3.5 py-1 text-sm text-text-2 transition-colors hover:text-text max-lg:min-h-11',
            s.id === selected && 'bg-night-2 text-text',
          )}
        >
          {s.name}
        </button>
      ))}
    </div>
  )
}

function ScanStatus({ inventory }: { inventory: AccountInventory }) {
  const { t } = useTranslation('admin')
  if (!inventory.scannedAt) {
    return <p className="mb-4 max-w-2xl text-base text-text-2">{t('accounts.neverScanned')}</p>
  }
  const state = inventory.scanError ? 'failed' : inventory.scanComplete ? 'complete' : 'incomplete'
  const tone = state === 'complete' ? 'ok' : state === 'failed' ? 'bad' : 'warn'
  return (
    <div className="mb-4 flex flex-col gap-1.5">
      <div className="flex flex-wrap items-center gap-3 text-sm text-text-2">
        <StatusChip tone={tone}>{t(`accounts.scanState.${state}`)}</StatusChip>
        <time
          className="type-data"
          dateTime={inventory.scannedAt}
          title={formatDateTime(inventory.scannedAt)}
        >
          {t('accounts.lastScan', { when: formatRelative(inventory.scannedAt) })}
        </time>
      </div>
      {state === 'incomplete' && (
        <p className="max-w-2xl text-sm text-text-3">{t('accounts.incompleteHint')}</p>
      )}
      {inventory.scanError && (
        <p className="max-w-2xl text-sm text-text-3">
          {t('accounts.failedHint', {
            reason: t(serviceErrorKey(inventory.scanError), {
              defaultValue: t('services.errors.unknown'),
            }),
          })}
        </p>
      )}
    </div>
  )
}

function Inventory({ service, inventory }: { service: Service; inventory: AccountInventory }) {
  const { t } = useTranslation('admin')
  return (
    <>
      <ScanStatus inventory={inventory} />
      {inventory.scannedAt && inventory.accounts.length === 0 && (
        <p className="text-text-3">{t('accounts.noAccounts')}</p>
      )}
      {inventory.accounts.length > 0 && (
        <>
          {/* Phones: one card per account, nothing hidden behind a scroll. */}
          <ul className="flex flex-col divide-y divide-line rounded-panel border border-line bg-night-1 md:hidden">
            {inventory.accounts.map((account) => (
              <li key={account.remoteId} className={cn('px-4 py-3', dimmed(account))}>
                <div className="flex items-start justify-between gap-3">
                  <AccountName account={account} />
                  <IgnoreButton serviceId={service.id} account={account} />
                </div>
                <div className="mt-2">
                  <AccountState account={account} />
                </div>
                <dl className="mt-2 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm">
                  <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
                    {t('accounts.columns.libraries')}
                  </dt>
                  <dd className="type-data text-text-2">
                    <Libraries account={account} />
                  </dd>
                  <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
                    {t('accounts.columns.linked')}
                  </dt>
                  <dd>
                    <Suggestion account={account} />
                  </dd>
                  <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
                    {t('accounts.columns.lastSeen')}
                  </dt>
                  <dd className="type-data text-text-2">
                    <LastSeen account={account} />
                  </dd>
                </dl>
              </li>
            ))}
          </ul>
          <div className="relative hidden overflow-x-auto rounded-panel border border-line bg-night-1 md:block">
            <table className="w-full border-separate border-spacing-0 min-w-[48rem] text-left text-base">
              <thead>
                <tr className="type-data text-xs tracking-wide text-text-3 uppercase [&>th]:border-b [&>th]:border-line">
                  <th className="px-4 py-3 font-medium">{t('accounts.columns.name')}</th>
                  <th className="px-4 py-3 font-medium">{t('accounts.columns.state')}</th>
                  <th className="px-4 py-3 font-medium">{t('accounts.columns.libraries')}</th>
                  <th className="px-4 py-3 font-medium">{t('accounts.columns.linked')}</th>
                  <th className="px-4 py-3 font-medium">{t('accounts.columns.lastSeen')}</th>
                  <th className="px-4 py-3 text-right font-medium">
                    <span className="sr-only">{t('accounts.columns.actions')}</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {inventory.accounts.map((account) => (
                  <tr
                    key={account.remoteId}
                    className={cn(
                      'align-middle [&:not(:last-child)>td]:border-b [&>td]:border-line hover:bg-night-2/40',
                      dimmed(account),
                    )}
                  >
                    <td className="px-4 py-3">
                      <AccountName account={account} />
                    </td>
                    <td className="px-4 py-3">
                      <AccountState account={account} />
                    </td>
                    <td className="type-data px-4 py-3 text-sm whitespace-nowrap text-text-2">
                      <Libraries account={account} />
                    </td>
                    <td className="px-4 py-3 text-sm whitespace-nowrap">
                      <Suggestion account={account} />
                    </td>
                    <td className="type-data px-4 py-3 text-sm whitespace-nowrap text-text-2">
                      <LastSeen account={account} />
                    </td>
                    <td className="px-2 py-2">
                      <div className="flex justify-end">
                        <IgnoreButton serviceId={service.id} account={account} />
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </>
  )
}

/** Gone and ignored accounts recede: Starr does nothing with them. */
function dimmed(account: ForeignAccount): string | false {
  if (account.goneSince) return 'opacity-70'
  return account.ignoredAt != null && 'opacity-85'
}

function chipHint(
  chip: string,
  account: ForeignAccount,
  t: ReturnType<typeof useTranslation<'admin'>>['t'],
): string | undefined {
  switch (chip) {
    case 'admin':
      return t('accounts.hint.admin')
    case 'locked':
      return t('accounts.hint.locked')
    case 'missing':
      return t('accounts.hint.missing')
    case 'gone':
      return account.goneSince
        ? t('accounts.hint.gone', { when: formatDateTime(account.goneSince) })
        : undefined
    case 'ignored':
      return account.ignoredAt
        ? t('accounts.hint.ignored', {
            admin: account.ignoredBy ?? '–',
            when: formatDateTime(account.ignoredAt),
          })
        : undefined
    default:
      return undefined
  }
}

function AccountName({ account }: { account: ForeignAccount }) {
  const quiet = account.goneSince != null || account.ignoredAt != null
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-1">
        <span className={cn('font-semibold', quiet ? 'text-text-2' : 'text-text')}>
          {account.username}
        </span>
        <CopyId id={account.remoteId} name={account.username} />
      </div>
    </div>
  )
}

function AccountState({ account }: { account: ForeignAccount }) {
  const { t } = useTranslation('admin')
  return (
    <div className="flex flex-wrap gap-1.5">
      {accountChips(account).map(({ chip, tone }) => (
        <span key={chip} title={chipHint(chip, account, t)}>
          <StatusChip tone={tone}>{t(`accounts.state.${chip}`)}</StatusChip>
        </span>
      ))}
    </div>
  )
}

function Libraries({ account }: { account: ForeignAccount }) {
  const { t } = useTranslation('admin')
  const ids = account.libraryIds
  if (ids == null) return t('accounts.libraries.all')
  if (ids.length === 0) return t('accounts.libraries.none')
  return t('accounts.libraries.some', { count: ids.length })
}

function Suggestion({ account }: { account: ForeignAccount }) {
  const { t } = useTranslation('admin')
  if (account.linkedMember) {
    return (
      <Link
        to={`/admin/people/members/${account.linkedMember.id}`}
        title={t('accounts.linkedHint')}
        className="font-medium text-text hover:underline"
      >
        <LinkIcon aria-hidden className="mr-1 inline size-3.5 text-text-3" />
        {account.linkedMember.username}
      </Link>
    )
  }
  const member = account.suggestedMember
  if (!member) return <span className="text-text-3">–</span>
  return (
    <Link
      to={`/admin/people/members/${member.id}`}
      title={t('accounts.suggestion.hint')}
      className="text-text hover:underline"
    >
      {t('accounts.suggestion.matches', { username: member.username })}
    </Link>
  )
}

function LastSeen({ account }: { account: ForeignAccount }) {
  return (
    <time dateTime={account.lastSeen} title={formatDateTime(account.lastSeen)}>
      {formatRelative(account.lastSeen)}
    </time>
  )
}

function IgnoreButton({ serviceId, account }: { serviceId: number; account: ForeignAccount }) {
  const { t } = useTranslation('admin')
  const setIgnored = useSetAccountIgnored(serviceId)
  const ignored = account.ignoredAt != null
  const toggle = async () => {
    try {
      await setIgnored.mutateAsync({ remoteId: account.remoteId, ignored: !ignored })
      toast(t(ignored ? 'accounts.unignored' : 'accounts.ignored', { name: account.username }))
    } catch {
      toast.error(t('accounts.errors.actionFailed'))
    }
  }
  const label = t(ignored ? 'accounts.unignore' : 'accounts.ignore')
  if (account.linkedMember) return null
  return (
    <Button
      variant="ghost"
      size="sm"
      disabled={setIgnored.isPending}
      onClick={toggle}
      aria-label={t(ignored ? 'accounts.unignoreFor' : 'accounts.ignoreFor', {
        name: account.username,
      })}
    >
      {ignored ? <EyeIcon aria-hidden /> : <EyeOffIcon aria-hidden />}
      {label}
    </Button>
  )
}
