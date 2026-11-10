import { useId } from 'react'
import { useTranslation } from 'react-i18next'

import { type MatrixService, useAccountMatrix, useAccounts } from '@/api/accounts'
import { cn } from '@/lib/utils'

/** Chosen accounts: service id → remote id. */
export type PresetChoice = Record<number, string>

/**
 * The friend's existing accounts an invitation names (spec 0005 M3): one
 * optional pick per media server. Accepting links them, password deferred.
 */
export function PresetAccountsPicker({
  value,
  onChange,
}: {
  value: PresetChoice
  onChange: (next: PresetChoice) => void
}) {
  const { t } = useTranslation('admin')
  const matrix = useAccountMatrix()
  const services = matrix.data?.services ?? []
  if (services.length === 0) return null
  return (
    <fieldset className="flex flex-col gap-3">
      <legend className="mb-1.5 text-sm font-medium text-text-2">
        {t('invitations.dialog.accounts')}
      </legend>
      <p className="-mt-1 text-sm text-text-3">{t('invitations.dialog.accountsHint')}</p>
      {services.map((s) => (
        <ServicePick
          key={s.id}
          service={s}
          value={value[s.id] ?? ''}
          onChange={(remoteId) => {
            const next = { ...value }
            if (remoteId) next[s.id] = remoteId
            else delete next[s.id]
            onChange(next)
          }}
        />
      ))}
    </fieldset>
  )
}

function ServicePick({
  service,
  value,
  onChange,
}: {
  service: MatrixService
  value: string
  onChange: (remoteId: string) => void
}) {
  const { t } = useTranslation('admin')
  const id = useId()
  const accounts = useAccounts(service.id)
  const linkable = (accounts.data?.accounts ?? []).filter(
    (a) => !a.privileged && a.ignoredAt == null && a.goneSince == null && a.linkedMember == null,
  )
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm text-text">
        {service.name}
      </label>
      <select
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={cn(
          'h-10 w-full cursor-pointer rounded-control border border-line bg-night-0 px-3 text-base text-text transition-colors outline-hidden',
          'hover:border-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-offset-0 focus-visible:outline-polaris/40 focus-visible:outline-solid',
        )}
      >
        <option value="">{t('invitations.dialog.accountNone')}</option>
        {linkable.map((a) => (
          <option key={a.remoteId} value={a.remoteId}>
            {a.username}
          </option>
        ))}
      </select>
    </div>
  )
}
