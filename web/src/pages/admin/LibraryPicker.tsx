import { LibraryIcon, RefreshCwIcon, TriangleAlertIcon } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { useScanAccounts } from '@/api/accounts'
import type { CategoryName, CategorySettings } from '@/api/categories'
import { useApiErrorMessage } from '@/api/errors'
import { useServiceLibraries, useSetRoleLibraries } from '@/api/libraries'
import { CategoryDot } from '@/components/CategoryDot'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { formatRelative } from '@/lib/format'
import { serviceErrorKey } from '@/lib/services'
import { libraryFits } from '@/lib/libraries'
import { showUndoToast } from '@/lib/undo'
import { cn } from '@/lib/utils'

type Consumer = CategorySettings['consumers'][number]

/**
 * The libraries of one media server that belong to a category (spec 0005
 * M4): mapped ones as a list, a hint while none are mapped, and a dialog to
 * pick from the server's library list.
 */
export function RoleLibraries({
  category,
  consumer,
}: {
  category: CategoryName
  consumer: Consumer
}) {
  const { t } = useTranslation(['admin', 'common'])
  const [open, setOpen] = useState(false)
  const mapped = consumer.libraries

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm text-text-3">{t('categories.libraries.label')}</span>
        {mapped.length === 0 ? (
          <StatusChip tone="warn">{t('categories.libraries.none')}</StatusChip>
        ) : (
          <ul className="flex flex-wrap gap-1.5" aria-label={t('categories.libraries.label')}>
            {mapped.map((l) => (
              <li
                key={l.id}
                className="inline-flex items-center gap-1.5 rounded-full border border-line bg-night-1 px-2.5 py-0.5 text-sm text-text"
              >
                <LibraryIcon aria-hidden className="size-3.5 text-text-3" />
                {l.name ?? (
                  <span className="text-text-3">
                    {t('categories.libraries.gone')} <span className="type-data">{l.id}</span>
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
        <Button variant="ghost" size="sm" onClick={() => setOpen(true)}>
          {t('categories.libraries.choose')}
        </Button>
      </div>
      {mapped.length === 0 && (
        <p role="note" className="flex items-start gap-2 text-sm text-text-2">
          <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-warn" />
          {t('categories.libraries.noneHint')}
        </p>
      )}
      {open && (
        <LibraryDialog category={category} consumer={consumer} onClose={() => setOpen(false)} />
      )}
    </div>
  )
}

function LibraryDialog({
  category,
  consumer,
  onClose,
}: {
  category: CategoryName
  consumer: Consumer
  onClose: () => void
}) {
  const { t } = useTranslation(['admin', 'common'])
  const errorMessage = useApiErrorMessage()
  // While a scan this dialog asked for runs, poll until a newer listing
  // arrived.
  const [scanSince, setScanSince] = useState<string | null>(null)
  const libraries = useServiceLibraries(consumer.serviceId, scanSince !== null)
  const listedAt = libraries.data?.listedAt
  if (scanSince !== null && listedAt != null && listedAt > scanSince) setScanSince(null)
  const save = useSetRoleLibraries()
  const scan = useScanAccounts()
  const [chosen, setChosen] = useState<Set<string>>(
    () => new Set(consumer.libraries.map((l) => l.id)),
  )
  const [failure, setFailure] = useState<string | null>(null)
  const list = libraries.data?.libraries ?? []
  // Mapped earlier but no longer listed: kept unless unticked.
  const gone = consumer.libraries.filter((m) => !list.some((l) => l.id === m.id))
  const toggle = (id: string, on: boolean) =>
    setChosen((current) => {
      const next = new Set(current)
      if (on) next.add(id)
      else next.delete(id)
      return next
    })

  return (
    <Dialog open onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {t('categories.libraries.dialogTitle', {
              category: t(`common:categories.${category}`),
              service: consumer.serviceName,
            })}
          </DialogTitle>
          <DialogDescription>{t('categories.libraries.dialogDescription')}</DialogDescription>
        </DialogHeader>
        <DialogBody>
          {libraries.isPending && <p className="text-text-3">{t('common:status.loading')}</p>}
          {libraries.data?.error && (
            <p role="alert" className="text-sm text-bad">
              {t('categories.libraries.listFailed', {
                reason: t(serviceErrorKey(libraries.data.error), {
                  defaultValue: t('services.errors.unknown'),
                }),
              })}
            </p>
          )}
          {libraries.isSuccess && list.length === 0 && gone.length === 0 && (
            <p className="text-sm text-text-3">{t('categories.libraries.notListed')}</p>
          )}

          {(list.length > 0 || gone.length > 0) && (
            <ul className="flex flex-col divide-y divide-line rounded-card border border-line bg-night-0">
              {list.map((l) => {
                const other = l.category && l.category !== category ? l.category : null
                const id = `library-${consumer.id}-${l.id}`
                return (
                  <li key={l.id} className="flex items-center gap-3 px-3 py-2.5">
                    <Checkbox
                      id={id}
                      checked={chosen.has(l.id)}
                      disabled={other !== null}
                      onCheckedChange={(on) => toggle(l.id, on === true)}
                    />
                    <label
                      htmlFor={id}
                      className={cn(
                        'flex min-w-0 flex-1 flex-col leading-tight',
                        other ? 'text-text-3' : 'cursor-pointer text-text',
                      )}
                    >
                      <span className="truncate font-medium">{l.name}</span>
                      <span className="text-sm text-text-3">
                        {t(`categories.libraries.content.${l.content}`)}
                      </span>
                    </label>
                    {!other && chosen.has(l.id) && !libraryFits(category, l.content) && (
                      // A books library under Movies is almost always a slip (ADM-19).
                      <span className="flex items-center gap-1.5 text-sm text-warn">
                        <TriangleAlertIcon aria-hidden className="size-3.5 shrink-0" />
                        {t('categories.libraries.mismatch', {
                          content: t(`categories.libraries.content.${l.content}`),
                          category: t(`common:categories.${category}`),
                        })}
                      </span>
                    )}
                    {other && (
                      <span className="flex items-center gap-1.5 text-sm text-text-3">
                        <CategoryDot category={other} />
                        {t('categories.libraries.takenBy', {
                          category: t(`common:categories.${other}`),
                        })}
                      </span>
                    )}
                  </li>
                )
              })}
              {gone.map((m) => {
                const id = `library-${consumer.id}-${m.id}`
                return (
                  <li key={m.id} className="flex items-center gap-3 px-3 py-2.5">
                    <Checkbox
                      id={id}
                      checked={chosen.has(m.id)}
                      onCheckedChange={(on) => toggle(m.id, on === true)}
                    />
                    <label htmlFor={id} className="flex flex-col leading-tight text-text-3">
                      <span>{t('categories.libraries.gone')}</span>
                      <span className="type-data text-sm">{m.id}</span>
                    </label>
                  </li>
                )
              })}
            </ul>
          )}

          <div className="flex flex-wrap items-center gap-2 text-sm text-text-3">
            {libraries.data?.listedAt && (
              <span>
                {t('categories.libraries.listedAt', {
                  when: formatRelative(libraries.data.listedAt),
                })}
              </span>
            )}
            <Button
              variant="ghost"
              size="sm"
              disabled={scan.isPending || scanSince !== null}
              onClick={() =>
                scan.mutate(undefined, {
                  onSuccess: () => {
                    setScanSince(new Date().toISOString())
                    toast(t('categories.libraries.scanQueued'))
                  },
                  onError: (error) => toast.error(errorMessage(error)),
                })
              }
            >
              <RefreshCwIcon aria-hidden />
              {t('categories.libraries.scan')}
            </Button>
          </div>

          {failure && (
            <p role="alert" className="text-sm text-bad">
              {failure}
            </p>
          )}
        </DialogBody>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            {t('common:actions.cancel')}
          </Button>
          <Button
            variant="primary"
            disabled={save.isPending}
            onClick={() => {
              const before = consumer.libraries.map((l) => l.id)
              // Promise callbacks: the dialog closes before the toast's undo is used.
              save.mutateAsync({ category, roleId: consumer.id, libraryIds: [...chosen] }).then(
                () => {
                  onClose()
                  // Reversible: undo puts the exact previous list back (ADM-19, F8).
                  showUndoToast({
                    message: t('categories.libraries.saved'),
                    onUndo: () =>
                      void save
                        .mutateAsync({ category, roleId: consumer.id, libraryIds: before })
                        .then(
                          () => toast(t('categories.libraries.restored')),
                          (error: unknown) => toast.error(errorMessage(error)),
                        ),
                  })
                },
                (error: unknown) => setFailure(errorMessage(error)),
              )
            }}
          >
            {t('categories.libraries.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
