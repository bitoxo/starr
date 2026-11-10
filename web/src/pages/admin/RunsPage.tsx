import { ActivityIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { useSearchParams } from 'react-router'
import { z } from 'zod'

import {
  type RunDetail,
  type RunOutcome,
  type RunState,
  type RunSummary,
  useRun,
  useRuns,
} from '@/api/runs'
import { PageHeader } from '@/components/AppShell'
import { type Column, DataTable } from '@/components/DataTable'
import { EmptyState } from '@/components/EmptyState'
import { QueryError } from '@/components/QueryError'
import { StatusChip, type StatusTone } from '@/components/StatusChip'
import { TechnicalDetails } from '@/components/TechnicalDetails'
import { Button } from '@/components/ui/button'
import { Sheet, SheetContent, SheetDescription, SheetTitle } from '@/components/ui/sheet'
import { formatDateTime, formatElapsed, formatRelative } from '@/lib/format'
import { serviceErrorKey } from '@/lib/services'
import { useUrlState } from '@/lib/urlState'

/*
 * Run history (ADM-29): job and outcome filters in the URL, readable job names, pages of 50 with
 * "Load more", the raw report behind a disclosure, errors as a sentence with the technical text
 * collapsible (ADM-17).
 */

const STATE_TONE: Record<RunState, StatusTone> = {
  running: 'info',
  succeeded: 'ok',
  failed: 'bad',
  aborted: 'warn',
}

/** Every job kind the server runs (`JobKind::ALL`), in its display order, with its name. */
const JOB_KINDS = {
  'system.housekeeping': 'housekeeping',
  'system.noop': 'noop',
  'services.check': 'servicesCheck',
  'sources.lists': 'sourcesLists',
  'inventory.sync': 'inventorySync',
  'requests.submit': 'requestsSubmit',
  'acquisitions.sync': 'acquisitionsSync',
  'notifications.deliver': 'notificationsDeliver',
  'follows.check': 'followsCheck',
  'accounts.scan': 'accountsScan',
  'accounts.reconcile': 'accountsReconcile',
  'accounts.password': 'accountsPassword',
  'catalog.refresh-lists': 'catalogRefreshLists',
} as const

type JobKind = keyof typeof JOB_KINDS
const KINDS = Object.keys(JOB_KINDS) as JobKind[]
const OUTCOMES = ['running', 'succeeded', 'attention', 'failed'] as const satisfies RunOutcome[]

function useKindLabel() {
  const { t } = useTranslation('admin')
  // A kind written by a newer release keeps its raw name.
  return (kind: string) => (kind in JOB_KINDS ? t(`runs.kind.${JOB_KINDS[kind as JobKind]}`) : kind)
}

/** Run state, and for succeeded runs whether the expectation was met. */
function RunStateChip({ run }: { run: RunSummary }) {
  const { t } = useTranslation('admin')
  if (run.state === 'succeeded' && run.expectation === 'unmet') {
    return <StatusChip tone="warn">{t('runs.expectation.unmet')}</StatusChip>
  }
  return <StatusChip tone={STATE_TONE[run.state]}>{t(`runs.state.${run.state}`)}</StatusChip>
}

const selectClass =
  'h-9 cursor-pointer rounded-full border border-line bg-night-1 px-3 text-sm text-text hover:border-text-3'

export default function RunsPage() {
  const { t } = useTranslation(['admin', 'common'])
  const kindLabel = useKindLabel()
  const [kind, setKind] = useUrlState<string>('job', z.enum(KINDS), '')
  const [outcome, setOutcome] = useUrlState<string>('outcome', z.enum(OUTCOMES), '')
  // One update for both: two setters in one tick would each start from the same URL.
  const [, setParams] = useSearchParams()
  const [selected, setSelected] = useUrlState('run', z.coerce.number().int().positive(), 0)
  const runs = useRuns({
    kind: kind || undefined,
    outcome: (outcome || undefined) as RunOutcome | undefined,
  })
  const list = runs.data?.pages.flatMap((p) => p.runs)
  const filtered = Boolean(kind || outcome)

  const columns: Column<RunSummary>[] = [
    {
      id: 'job',
      header: t('runs.columns.job'),
      className: 'min-w-56',
      cell: (run) => (
        <button
          type="button"
          className="cursor-pointer rounded-control text-left font-semibold text-text hover:underline"
          onClick={() => setSelected(run.id, { push: true })}
        >
          {kindLabel(run.kind)}
          <span className="type-data ml-2 text-sm font-normal text-text-3">#{run.id}</span>
        </button>
      ),
    },
    {
      id: 'state',
      header: t('runs.columns.state'),
      className: 'w-40',
      cell: (run) => <RunStateChip run={run} />,
    },
    {
      id: 'trigger',
      header: t('runs.columns.trigger'),
      className: 'w-40',
      cell: (run) => (
        <span className="text-sm text-text-2">
          {t(`runs.trigger.${run.trigger}`)}
          {run.attempt > 1 && (
            <span className="text-text-3">
              {' · '}
              {t('runs.attempt', { n: run.attempt })}
            </span>
          )}
        </span>
      ),
    },
    {
      id: 'started',
      header: t('runs.columns.started'),
      className: 'w-36',
      cell: (run) => (
        <time
          dateTime={run.startedAt}
          title={formatDateTime(run.startedAt)}
          className="type-data text-sm text-text-2"
        >
          {formatRelative(run.startedAt)}
        </time>
      ),
    },
    {
      id: 'duration',
      header: t('runs.columns.duration'),
      align: 'end',
      className: 'w-28',
      cell: (run) => (
        <span className="type-data text-sm text-text-2">
          {run.finishedAt ? formatElapsed(run.startedAt, run.finishedAt) : '…'}
        </span>
      ),
    },
  ]

  const filters = (
    <>
      <label className="flex items-center gap-2 text-sm text-text-2">
        {t('runs.filters.job')}
        <select value={kind} onChange={(e) => setKind(e.target.value)} className={selectClass}>
          <option value="">{t('runs.filters.allJobs')}</option>
          {KINDS.map((k) => (
            <option key={k} value={k}>
              {kindLabel(k)}
            </option>
          ))}
        </select>
      </label>
      <label className="flex items-center gap-2 text-sm text-text-2">
        {t('runs.filters.outcome')}
        <select
          value={outcome}
          onChange={(e) => setOutcome(e.target.value)}
          className={selectClass}
        >
          <option value="">{t('runs.filters.allOutcomes')}</option>
          {OUTCOMES.map((o) => (
            <option key={o} value={o}>
              {t(`runs.filters.outcomes.${o}`)}
            </option>
          ))}
        </select>
      </label>
      {filtered && (
        <Button
          size="sm"
          variant="ghost"
          onClick={() =>
            setParams(
              (p) => {
                p.delete('job')
                p.delete('outcome')
                return p
              },
              { replace: true },
            )
          }
        >
          {t('runs.filters.clear')}
        </Button>
      )}
    </>
  )

  return (
    <>
      <PageHeader variant="admin" title={t('runs.title')} description={t('runs.description')} />

      {runs.isSuccess && list?.length === 0 && !filtered ? (
        <EmptyState
          icon={ActivityIcon}
          title={t('runs.emptyTitle')}
          description={t('runs.emptyDescription')}
        />
      ) : (
        <DataTable
          label={t('runs.title')}
          columns={columns}
          rows={list}
          rowKey={(run) => run.id}
          filters={filters}
          loading={runs.isPending}
          error={runs.isError ? t('runs.loadFailed') : null}
          onRetry={() => void runs.refetch()}
          empty={t('runs.noMatches')}
        />
      )}
      {runs.hasNextPage && (
        <Button
          className="mt-4"
          disabled={runs.isFetchingNextPage}
          onClick={() => void runs.fetchNextPage()}
        >
          {t('runs.loadMore')}
        </Button>
      )}

      <Sheet open={selected !== 0} onOpenChange={(open) => !open && setSelected(0)}>
        <SheetContent side="right" className="w-full max-w-xl overflow-y-auto sm:max-w-xl">
          {selected !== 0 && <RunDetailPanel id={selected} />}
        </SheetContent>
      </Sheet>
    </>
  )
}

function RunDetailPanel({ id }: { id: number }) {
  const { t } = useTranslation('admin')
  const kindLabel = useKindLabel()
  const run = useRun(id)

  if (!run.data) {
    return (
      <div className="flex flex-col gap-4 p-6">
        <SheetTitle className="type-display text-xl font-bold">
          {t('runs.detail.title', { id })}
        </SheetTitle>
        <SheetDescription className="text-text-3">
          {run.isError ? t('runs.detail.loadFailed') : t('runs.detail.loading')}
        </SheetDescription>
        {run.isError && <QueryError onRetry={() => void run.refetch()} />}
      </div>
    )
  }
  const detail: RunDetail = run.data
  const report = detail.report

  return (
    <div className="flex flex-col gap-6 p-6 pr-12">
      <div className="flex flex-col gap-2">
        <SheetTitle className="type-display text-xl font-bold">
          {kindLabel(detail.kind)}
          <span className="type-data ml-2 text-base font-normal text-text-3">#{detail.id}</span>
        </SheetTitle>
        <SheetDescription asChild>
          <div>
            <RunStateChip run={detail} />
          </div>
        </SheetDescription>
      </div>

      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
        <Fact label={t('runs.detail.trigger')}>
          {t(`runs.trigger.${detail.trigger}`)} · {t('runs.attempt', { n: detail.attempt })}
        </Fact>
        <Fact label={t('runs.detail.started')}>{formatDateTime(detail.startedAt)}</Fact>
        {detail.finishedAt && (
          <>
            <Fact label={t('runs.detail.finished')}>{formatDateTime(detail.finishedAt)}</Fact>
            <Fact label={t('runs.detail.duration')}>
              {formatElapsed(detail.startedAt, detail.finishedAt)}
            </Fact>
          </>
        )}
        <Fact label={t('runs.columns.job')}>{t('runs.detail.job', { id: detail.jobId })}</Fact>
      </dl>

      {detail.error && (
        <Section title={t('runs.detail.error')}>
          <p className="text-sm text-text">{t(`runs.failedPlain.${detail.state}`)}</p>
          <TechnicalDetails text={detail.error} />
        </Section>
      )}

      {report ? (
        <>
          <Section title={t('runs.detail.summary')}>
            <p className="text-sm text-text-2">
              {t('runs.detail.counts', {
                succeeded: report.succeeded,
                failed: report.failed,
                skipped: report.skipped,
                total: report.total,
              })}
            </p>
            {report.expectation.detail && (
              <p className="text-sm text-text">{report.expectation.detail}</p>
            )}
          </Section>

          {report.items && report.items.length > 0 && (
            <Section title={t('runs.detail.items')}>
              <ul className="flex flex-col divide-y divide-line rounded-card border border-line">
                {report.items.map((item, index) => (
                  <li key={index} className="flex flex-col gap-1 px-3 py-2.5">
                    <div className="flex items-center justify-between gap-3">
                      <span className="font-medium text-text">{item.target}</span>
                      <StatusChip
                        tone={
                          item.outcome === 'succeeded'
                            ? 'ok'
                            : item.outcome === 'failed'
                              ? 'bad'
                              : 'neutral'
                        }
                      >
                        {t(`runs.outcome.${item.outcome}`)}
                      </StatusChip>
                    </div>
                    {item.code && (
                      <p className="text-sm text-text-2">
                        {t(serviceErrorKey(item.code), { defaultValue: item.code })}
                      </p>
                    )}
                    {item.detail && <TechnicalDetails text={item.detail} />}
                  </li>
                ))}
              </ul>
            </Section>
          )}

          <details className="text-sm">
            <summary className="type-data cursor-pointer rounded-control text-xs tracking-wide text-text-3 uppercase hover:text-text">
              {t('runs.detail.report')}
            </summary>
            <pre className="type-data mt-2 max-h-80 overflow-auto rounded-card border border-line bg-night-0 p-3 text-xs leading-relaxed text-text-2">
              {JSON.stringify(report, null, 2)}
            </pre>
          </details>
        </>
      ) : (
        detail.state !== 'running' && (
          <p className="text-sm text-text-3">{t('runs.detail.noReport')}</p>
        )
      )}

      <Section title={t('runs.detail.transitions')}>
        <ol className="flex flex-col gap-2 border-l border-line pl-4">
          {detail.transitions.map((tr, index) => (
            <li key={index} className="relative text-sm">
              <span
                aria-hidden
                className="absolute top-1.5 -left-[1.3rem] size-2 rounded-full border border-line bg-night-2"
              />
              <span className="text-text">
                {tr.from ? `${t(`runs.state.${tr.from}`)} → ` : `${t('runs.detail.created')} → `}
                {t(`runs.state.${tr.to}`)}
              </span>
              <span className="type-data ml-2 text-xs text-text-3">{formatDateTime(tr.at)}</span>
              {tr.note && <p className="text-xs text-text-3">{tr.note}</p>}
            </li>
          ))}
        </ol>
      </Section>
    </div>
  )
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="type-data text-xs tracking-wide text-text-3 uppercase">{label}</dt>
      <dd className="text-text">{children}</dd>
    </>
  )
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2">
      <h3 className="type-data text-xs tracking-wide text-text-3 uppercase">{title}</h3>
      {children}
    </section>
  )
}
