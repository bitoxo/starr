import { PencilIcon, PlusIcon, RefreshCwIcon, ServerIcon, Trash2Icon } from 'lucide-react'
import { type ReactNode, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { toast } from 'sonner'

import { useJobRun } from '@/api/runs'
import { useApiErrorMessage } from '@/api/errors'
import {
  type HeldAcquisitions,
  type HeldTitle,
  type Service,
  useCheckService,
  useFetchHeld,
  useHeld,
  useServiceTypes,
  useServices,
  useSetWritesPaused,
} from '@/api/services'
import { PageHeader } from '@/components/AppShell'
import { type Column, DataTable } from '@/components/DataTable'
import { EmptyState } from '@/components/EmptyState'
import { ServiceStatusChip } from '@/components/ServiceStatusChip'
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
import { Switch } from '@/components/ui/switch'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { formatDateTime, formatRelative } from '@/lib/format'
import { serviceErrorKey, serviceStatus } from '@/lib/services'
import { useUrlSort } from '@/lib/urlState'

import { DeleteServiceDialog, ServiceFormDialog } from './ServiceDialogs'

export default function ServicesPage() {
  const { t } = useTranslation(['admin', 'common'])
  const services = useServices()
  const types = useServiceTypes()
  const check = useCheckService()
  const [dialog, setDialog] = useState<{ open: boolean; service?: Service }>({ open: false })
  const [deleting, setDeleting] = useState<Service | null>(null)
  const [sort, setSort] = useUrlSort()
  // Checks queued from the table: service id → job id, until the run finished.
  const [checking, setChecking] = useState<Record<number, number>>({})
  const finishCheck = (serviceId: number) => {
    setChecking((current) => {
      const next = { ...current }
      delete next[serviceId]
      return next
    })
    void services.refetch()
  }

  const type = (id: string) => types.data?.find((ty) => ty.typeId === id)
  const typeName = (id: string) => type(id)?.displayName ?? id
  const has = (id: string, capability: string) =>
    type(id)?.capabilities.includes(capability) ?? false
  const addButton = (
    <Button variant="primary" onClick={() => setDialog({ open: true })}>
      <PlusIcon aria-hidden />
      {t('services.add')}
    </Button>
  )

  // Fixed widths so an error sentence never reflows the table (ADM-35).
  const columns: Column<Service>[] = [
    {
      id: 'name',
      header: t('services.columns.name'),
      className: 'w-56 min-w-56',
      sortValue: (s) => s.name,
      cell: (service) => (
        <div className="min-w-0">
          <button
            type="button"
            onClick={() => setDialog({ open: true, service })}
            className="cursor-pointer text-left font-semibold break-words text-text hover:underline"
          >
            {service.name}
          </button>
          <div className="text-sm text-text-3">{typeName(service.adapterType)}</div>
        </div>
      ),
    },
    {
      id: 'url',
      header: t('services.columns.url'),
      className: 'w-60 max-w-60',
      cell: (service) => (
        <span className="type-data block truncate text-sm text-text-2" title={service.baseUrl}>
          {service.baseUrl}
        </span>
      ),
    },
    {
      id: 'version',
      header: t('services.columns.version'),
      className: 'w-28',
      cell: (service) => (
        <span className="type-data text-sm text-text">
          {service.detectedVersion ?? <span className="text-text-3">–</span>}
        </span>
      ),
    },
    {
      id: 'status',
      header: t('services.columns.status'),
      className: 'w-64 min-w-64',
      sortValue: (s) => serviceStatus(s),
      cell: (service) =>
        checking[service.id] ? (
          <StatusChip tone="info">{t('services.status.pending')}</StatusChip>
        ) : (
          <div className="flex flex-col items-start gap-1">
            <ServiceStatusChip service={service} />
            {service.lastError && (
              <p className="text-sm text-text-3">
                {t(serviceErrorKey(service.lastError), {
                  defaultValue: t('services.errors.unknown'),
                })}
              </p>
            )}
          </div>
        ),
    },
    {
      id: 'lastCheck',
      header: t('services.columns.lastCheck'),
      className: 'w-32',
      sortValue: (s) => s.lastCheckAt,
      cell: (service) =>
        service.lastCheckAt ? (
          <time
            dateTime={service.lastCheckAt}
            title={formatDateTime(service.lastCheckAt)}
            className="type-data text-sm text-text-2"
          >
            {formatRelative(service.lastCheckAt)}
          </time>
        ) : (
          <span className="text-sm text-text-3">{t('services.never')}</span>
        ),
    },
    {
      id: 'writes',
      header: t('services.columns.writes'),
      className: 'w-56 min-w-56',
      cell: (service) =>
        has(service.adapterType, 'requests') ? (
          <WritesToggle service={service} />
        ) : has(service.adapterType, 'accounts') ? (
          // Account writes live with the plan they apply (ADM-15, interim).
          <Link
            to={`/admin/media/accounts?service=${service.id}`}
            className="rounded-control text-sm text-text-2 underline decoration-line underline-offset-2 hover:text-text"
          >
            {t('services.writes.inAccounts')}
          </Link>
        ) : (
          <span className="text-sm text-text-3">{t('services.writes.readOnly')}</span>
        ),
    },
    {
      id: 'actions',
      header: t('services.columns.actions'),
      align: 'end',
      className: 'w-36',
      cell: (service) => (
        <div className="flex justify-end gap-0.5">
          <IconAction
            label={t('services.actions.check')}
            name={service.name}
            disabled={checking[service.id] !== undefined}
            onClick={async () => {
              try {
                const jobId = await check.mutateAsync(service.id)
                setChecking((c) => ({ ...c, [service.id]: jobId }))
                toast(t('services.actions.checkQueued', { name: service.name }))
              } catch {
                toast.error(t('services.errors.unavailable'))
              }
            }}
          >
            <RefreshCwIcon aria-hidden />
          </IconAction>
          <IconAction
            label={t('services.actions.edit')}
            name={service.name}
            onClick={() => setDialog({ open: true, service })}
          >
            <PencilIcon aria-hidden />
          </IconAction>
          <IconAction
            label={t('services.actions.delete')}
            name={service.name}
            className="hover:text-bad"
            onClick={() => setDeleting(service)}
          >
            <Trash2Icon aria-hidden />
          </IconAction>
        </div>
      ),
    },
  ]

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('services.title')}
        description={t('services.description')}
        actions={services.data?.length ? addButton : undefined}
      />

      {services.isSuccess && services.data.length === 0 ? (
        <EmptyState
          icon={ServerIcon}
          title={t('services.emptyTitle')}
          description={t('services.emptyDescription')}
          action={addButton}
        />
      ) : (
        <DataTable
          label={t('services.title')}
          columns={columns}
          rows={services.data}
          rowKey={(s) => s.id}
          sort={sort}
          onSortChange={setSort}
          loading={services.isPending}
          error={services.isError ? t('services.errors.unavailable') : null}
          onRetry={() => void services.refetch()}
        />
      )}

      {Object.entries(checking).map(([serviceId, jobId]) => (
        <JobWatcher key={jobId} jobId={jobId} onDone={() => finishCheck(Number(serviceId))} />
      ))}
      <ServiceFormDialog
        service={dialog.service}
        open={dialog.open}
        onOpenChange={(open) => setDialog((d) => ({ ...d, open }))}
      />
      <DeleteServiceDialog service={deleting} onOpenChange={(open) => !open && setDeleting(null)} />
    </>
  )
}

/** An icon button with a visible tooltip on hover and focus (ADM-35); named per service. */
function IconAction({
  label,
  name,
  className,
  disabled,
  onClick,
  children,
}: {
  label: string
  name: string
  className?: string
  disabled?: boolean
  onClick: () => void
  children: ReactNode
}) {
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            aria-label={`${label}: ${name}`}
            className={className}
            disabled={disabled}
            onClick={onClick}
          >
            {children}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{label}</TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}

/**
 * Writes to a source on or paused (spec 0003 O5). While paused, approved
 * requests wait as held; resuming sends them out.
 */
function WritesToggle({ service }: { service: Service }) {
  const { t } = useTranslation('admin')
  const errorMessage = useApiErrorMessage()
  const setPaused = useSetWritesPaused()
  const fetchHeld = useFetchHeld()
  const [checking, setChecking] = useState(false)
  // Held titles waiting for the admin's confirmation before resuming.
  const [confirm, setConfirm] = useState<HeldAcquisitions | null>(null)
  const paused = service.writesPausedAt != null
  // How much waits there, right at the switch (ADM-15, interim until one writes model).
  const held = useHeld(service.id, paused).data

  const apply = async (on: boolean) => {
    try {
      await setPaused.mutateAsync({ id: service.id, paused: !on })
      setConfirm(null)
      toast(
        t(on ? 'services.writes.resumedToast' : 'services.writes.pausedToast', {
          name: service.name,
        }),
      )
    } catch (err) {
      toast.error(errorMessage(err))
    }
  }

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2 text-sm text-text-2">
        <Switch
          checked={!paused}
          disabled={setPaused.isPending || checking}
          aria-label={t('services.writes.label', { name: service.name })}
          onCheckedChange={async (on) => {
            if (!on) return apply(false)
            // Only ask when resuming sends something out.
            setChecking(true)
            try {
              const held = await fetchHeld(service.id)
              if (held.total > 0 || held.stopTotal > 0) setConfirm(held)
              else await apply(true)
            } catch (err) {
              toast.error(errorMessage(err))
            } finally {
              setChecking(false)
            }
          }}
        />
        <ResumeWritesDialog
          name={service.name}
          held={confirm}
          pending={setPaused.isPending}
          onConfirm={() => apply(true)}
          onCancel={() => setConfirm(null)}
        />
        {paused ? (
          <StatusChip tone="warn">{t('services.writes.chip')}</StatusChip>
        ) : (
          t('services.writes.on')
        )}
      </div>
      {paused && held && held.total > 0 && (
        <span className="text-xs text-warn">
          {t('services.writes.heldCount', { count: held.total })}
        </span>
      )}
      {paused && service.writesPausedAt && (
        <span className="text-xs text-text-3" title={formatDateTime(service.writesPausedAt)}>
          {t('services.writes.pausedBy', {
            name: service.writesPausedBy ?? '–',
            when: formatRelative(service.writesPausedAt),
          })}
        </span>
      )}
    </div>
  )
}

/** "Severance S03", "The Matrix", "Radiohead – OK Computer": a held title as one line. */
function heldLabel(title: HeldTitle): string {
  if (title.album) return `${title.title} – ${title.album}`
  return title.season != null
    ? `${title.title} S${String(title.season).padStart(2, '0')}`
    : title.title
}

/**
 * Confirms resuming writes when approved titles or stops are held (spec 0003 O5,
 * spec 0004: a stop turns off the monitoring Starr switched on for a title nobody
 * wants any more).
 */
export function ResumeWritesDialog({
  name,
  held,
  pending,
  onConfirm,
  onCancel,
}: {
  /** The source's name. */
  name: string
  held: HeldAcquisitions | null
  pending: boolean
  onConfirm: () => void
  onCancel: () => void
}) {
  const { t } = useTranslation('admin')
  const more = held ? held.total - held.preview.length : 0
  const moreStops = held ? held.stopTotal - held.stops.length : 0
  return (
    <Dialog open={held !== null} onOpenChange={(open) => !open && !pending && onCancel()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('services.writes.confirmTitle', { name })}</DialogTitle>
          <DialogDescription>
            {held && held.total > 0
              ? t('services.writes.confirmDescription', { count: held.total, name })
              : t('services.writes.confirmStopsOnly', { name })}
          </DialogDescription>
        </DialogHeader>
        <DialogBody>
          {held && held.total > 0 && (
            <ul className="flex flex-col gap-1.5 rounded-card border border-line bg-night-0 px-4 py-3 text-base text-text">
              {held.preview.map((title) => (
                <li key={`${title.title}-${title.year}-${title.season}`} className="truncate">
                  {heldLabel(title)}
                </li>
              ))}
              {more > 0 && (
                <li className="text-text-3">{t('services.writes.confirmMore', { count: more })}</li>
              )}
            </ul>
          )}
          {held && held.stopTotal > 0 && (
            <div className="flex flex-col gap-2">
              <p className="text-sm text-text-2">
                {t('services.writes.confirmStops', { count: held.stopTotal, name })}
              </p>
              <ul className="flex flex-col gap-1.5 rounded-card border border-line bg-night-0 px-4 py-3 text-base text-text">
                {held.stops.map((title) => (
                  <li
                    key={`stop-${title.title}-${title.year}-${title.season}`}
                    className="flex items-baseline gap-2 truncate"
                  >
                    <span className="type-data shrink-0 text-xs text-text-3 uppercase">
                      {t('services.writes.stopLine')}
                    </span>
                    <span className="truncate">{heldLabel(title)}</span>
                  </li>
                ))}
                {moreStops > 0 && (
                  <li className="text-text-3">
                    {t('services.writes.confirmMore', { count: moreStops })}
                  </li>
                )}
              </ul>
            </div>
          )}
        </DialogBody>
        <DialogFooter>
          <Button variant="ghost" disabled={pending} onClick={onCancel}>
            {t('services.writes.confirmCancel')}
          </Button>
          <Button variant="primary" disabled={pending} onClick={onConfirm}>
            {t('services.writes.confirmResume')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** Renders nothing; calls `onDone` once the job's run finished. */
function JobWatcher({ jobId, onDone }: { jobId: number; onDone: () => void }) {
  const run = useJobRun(jobId).data
  const finished = run != null && run.state !== 'running'
  useEffect(() => {
    if (finished) onDone()
    // onDone changes every render; the transition to finished is what matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finished])
  return null
}
