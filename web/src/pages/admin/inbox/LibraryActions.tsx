import { EyeOffIcon, TriangleAlertIcon } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { type CategoryName, useCategories } from '@/api/categories'
import { useApiErrorMessage } from '@/api/errors'
import { type UnmappedLibrary, useSetLibraryIgnored, useSetRoleLibraries } from '@/api/libraries'
import { SelectField } from '@/components/MultiSelect'
import { Button } from '@/components/ui/button'
import { ATTENTION_KEY } from '@/lib/attention'
import { libraryFits } from '@/lib/libraries'
import { showUndoToast } from '@/lib/undo'
import { cn } from '@/lib/utils'

import { Actions } from './Actions'

/**
 * A library no category maps (spec 0005 M4): assign it to a category that watches in its
 * server, or say it belongs to none. Resolved in the row.
 */
export function LibraryActions({
  library,
  primary,
  onResolved,
  children,
}: {
  library: UnmappedLibrary
  primary: boolean
  onResolved: () => void
  children?: ReactNode
}) {
  const { t } = useTranslation(['admin', 'common'])
  const errorMessage = useApiErrorMessage()
  const categoriesQuery = useCategories()
  const categories = categoriesQuery.data ?? []
  const client = useQueryClient()
  const assign = useSetRoleLibraries()
  const ignore = useSetLibraryIgnored()
  const [choice, setChoice] = useState('')
  const [error, setError] = useState<string | null>(null)
  // Categories that watch in this server, with their current mapping.
  const options = categories.flatMap((c) =>
    c.consumers
      .filter((r) => r.serviceId === library.serviceId)
      .map((r) => ({ category: c.category, role: r })),
  )
  const picked = options.find((o) => o.category === choice)
  const busy = assign.isPending || ignore.isPending

  const runAssign = async () => {
    if (!picked) return
    const category: CategoryName = picked.category
    setError(null)
    // The saved list replaces the mapping: start from a fresh one, not a cached copy another
    // admin may have changed meanwhile.
    const fresh = (await categoriesQuery.refetch()).data
      ?.find((c) => c.category === category)
      ?.consumers.find((r) => r.id === picked.role.id)
    if (!fresh) return
    const before = fresh.libraries.map((m) => m.id)
    // Promise callbacks: the row goes away once resolved, the undo outlives it.
    assign
      .mutateAsync({ category, roleId: fresh.id, libraryIds: [...before, library.libraryId] })
      .then(
        () => {
          onResolved()
          // Reversible: undo restores the category's exact previous list (ADM-07, ADM-19).
          showUndoToast({
            message: t('needsYou.libraries.assigned', {
              name: library.name,
              category: t(`common:categories.${category}`),
            }),
            onUndo: () =>
              void assign.mutateAsync({ category, roleId: fresh.id, libraryIds: before }).then(
                () => {
                  // The row comes back now, not on the next poll.
                  void client.invalidateQueries({ queryKey: ATTENTION_KEY })
                  toast(t('categories.libraries.restored'))
                },
                (e: unknown) => toast.error(errorMessage(e)),
              ),
          })
        },
        (e: unknown) => setError(errorMessage(e)),
      )
  }
  const mismatch = picked && !libraryFits(picked.category, library.content)

  return (
    <>
      {options.length === 0 ? (
        <p className="basis-full text-sm text-text-3">{t('needsYou.libraries.noRole')}</p>
      ) : (
        <div className={cn('flex flex-wrap gap-2', primary ? 'items-end' : 'items-center')}>
          <div className="min-w-44">
            <SelectField
              // In a row the select sits on the line with its buttons (centred); the panel
              // shows the label above it.
              hideLabel={!primary}
              label={t('needsYou.libraries.assignTo', { name: library.name })}
              value={choice}
              onChange={setChoice}
            >
              <option value="">{t('needsYou.libraries.choose')}</option>
              {options.map((o) => (
                <option key={o.category} value={o.category}>
                  {t(`common:categories.${o.category}`)}
                </option>
              ))}
            </SelectField>
          </div>
          {mismatch && (
            <p
              role="note"
              className={cn('flex items-center gap-1.5 text-sm text-warn', primary && 'pb-2')}
            >
              <TriangleAlertIcon aria-hidden className="size-3.5 shrink-0" />
              {t('categories.libraries.mismatch', {
                content: t(`categories.libraries.content.${library.content}`),
                category: t(`common:categories.${picked.category}`),
              })}
            </p>
          )}
        </div>
      )}
      <Actions>
        {options.length > 0 && (
          <Button
            size="sm"
            variant={primary ? 'primary' : 'ghost'}
            disabled={!picked || busy}
            onClick={() => void runAssign()}
          >
            {t('needsYou.libraries.assign')}
          </Button>
        )}
        <Button
          size="sm"
          variant="ghost"
          disabled={busy}
          aria-label={t('needsYou.libraries.ignoreOne', { name: library.name })}
          onClick={() => {
            const target = { serviceId: library.serviceId, libraryId: library.libraryId }
            ignore.mutateAsync({ ...target, ignored: true }).then(
              () => {
                onResolved()
                // Reversible: undo makes it unmapped again (ADM-07).
                showUndoToast({
                  message: t('needsYou.libraries.ignored', { name: library.name }),
                  onUndo: () =>
                    void ignore.mutateAsync({ ...target, ignored: false }).then(
                      () => void client.invalidateQueries({ queryKey: ATTENTION_KEY }),
                      (e: unknown) => toast.error(errorMessage(e)),
                    ),
                })
              },
              (e: unknown) => setError(errorMessage(e)),
            )
          }}
        >
          <EyeOffIcon aria-hidden />
          {t('needsYou.libraries.ignore')}
        </Button>
        {children}
      </Actions>
      {error && (
        <p role="alert" className="basis-full text-sm text-bad">
          {t('inbox.rowError', { message: error })}
        </p>
      )}
    </>
  )
}
