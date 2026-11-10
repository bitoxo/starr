import { CheckIcon, CircleAlertIcon, CopyIcon, TriangleAlertIcon } from 'lucide-react'
import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import {
  absoluteUrl,
  CATEGORIES,
  type Entitlement,
  type Member,
  type RequestMode,
} from '@/api/members'
import { ConfirmDialog } from '@/components/ConfirmDialog'
import { CategoryDot } from '@/components/CategoryDot'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { useScrollIntoDialogView } from '@/lib/dialogView'
import { cn } from '@/lib/utils'

/** Status as chip: state in form and colour. */
export function MemberStatusChip({ member }: { member: Pick<Member, 'status' | 'passwordSet'> }) {
  const { t } = useTranslation()
  if (member.status === 'suspended')
    return <StatusChip tone="warn">{t('memberStatus.suspended')}</StatusChip>
  if (member.status === 'retired')
    return <StatusChip tone="bad">{t('memberStatus.retired')}</StatusChip>
  if (!member.passwordSet)
    return <StatusChip tone="info">{t('memberStatus.linkPending')}</StatusChip>
  return <StatusChip tone="ok">{t('memberStatus.active')}</StatusChip>
}

/** Enabled categories as dots with labels (adult shown as "NSFW"). */
export function CategoryList({
  entitlements,
  empty,
  modes = false,
}: {
  entitlements: Entitlement[]
  empty: string
  /** Label both request modes, not only "Auto" (admin lists, ADM-25). */
  modes?: boolean
}) {
  const { t } = useTranslation()
  const enabled = entitlements.filter((e) => e.enabled)
  if (enabled.length === 0) return <span className="text-sm text-text-3">{empty}</span>
  return (
    <ul className="flex flex-wrap items-center gap-x-3 gap-y-1">
      {enabled.map((e) => (
        <li key={e.category} className="flex items-center gap-1.5 text-sm text-text-2">
          <CategoryDot category={e.category} />
          {t(`categories.${e.category}`)}
          {(modes || e.requestMode === 'auto') && (
            <span className="type-data text-xs text-text-3">
              {t(`requestModes.${e.requestMode}`)}
            </span>
          )}
        </li>
      ))}
    </ul>
  )
}

/** Per category: on/off plus request mode. Adult starts off and is labelled as admin-only. */
export function EntitlementsEditor({
  value,
  onChange,
  adultHint,
  disabled,
  memberName,
}: {
  value: Entitlement[]
  onChange: (next: Entitlement[]) => void
  adultHint?: string
  disabled?: boolean
  /** Named in the NSFW confirmation; absent for a member not created yet. */
  memberName?: string
}) {
  const { t } = useTranslation(['admin', 'common'])
  const [pendingAdult, setPendingAdult] = useState<Entitlement[] | null>(null)
  const byCategory = new Map(value.map((e) => [e.category, e]))
  const update = (next: Entitlement) => {
    const list = CATEGORIES.map((c) =>
      c === next.category ? next : (byCategory.get(c) ?? fallback(c)),
    )
    // NSFW asks first, in the words of the operator statement (ADM-12).
    const adultBefore = byCategory.get('adult')?.enabled ?? false
    if (next.category === 'adult' && next.enabled && !adultBefore) setPendingAdult(list)
    else onChange(list)
  }
  const who = memberName ?? t('admin:memberDetail.adultConfirm.thisMember')

  return (
    <>
      <ConfirmDialog
        open={pendingAdult !== null}
        onOpenChange={(open) => !open && setPendingAdult(null)}
        title={t('admin:memberDetail.adultConfirm.title', { name: who })}
        description={t('admin:memberDetail.adultConfirm.text', { name: who })}
        confirmLabel={t('admin:memberDetail.adultConfirm.confirm')}
        onConfirm={() => {
          if (pendingAdult) onChange(pendingAdult)
          setPendingAdult(null)
        }}
      />
      <ul className="flex flex-col divide-y divide-line rounded-card border border-line">
        {CATEGORIES.map((category) => {
          const entitlement = byCategory.get(category) ?? fallback(category)
          const label = t(`common:categories.${category}`)
          return (
            <li key={category} className="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3">
              <Switch
                checked={entitlement.enabled}
                disabled={disabled}
                onCheckedChange={(enabled) => update({ ...entitlement, enabled })}
                aria-label={t('admin:memberDetail.enabledLabel', { category: label })}
              />
              <div className="flex min-w-0 flex-1 flex-col">
                <span className="flex items-center gap-2 text-base text-text">
                  <CategoryDot category={category} />
                  {label}
                </span>
                {category === 'adult' && adultHint && (
                  <span className="text-sm text-text-3">{adultHint}</span>
                )}
              </div>
              <ModeToggle
                value={entitlement.requestMode}
                disabled={disabled || !entitlement.enabled}
                label={t('admin:memberDetail.modeLabel', { category: label })}
                onChange={(requestMode) => update({ ...entitlement, requestMode })}
              />
            </li>
          )
        })}
      </ul>
    </>
  )
}

function fallback(category: Entitlement['category']): Entitlement {
  return { category, enabled: false, requestMode: 'approval' }
}

function ModeToggle({
  value,
  onChange,
  disabled,
  label,
}: {
  value: RequestMode
  onChange: (mode: RequestMode) => void
  disabled?: boolean
  label: string
}) {
  const { t } = useTranslation()
  const modes: RequestMode[] = ['approval', 'auto']
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={cn(
        'inline-flex rounded-full border border-line bg-night-0 p-0.5',
        disabled && 'opacity-50',
      )}
    >
      {modes.map((mode) => (
        <button
          key={mode}
          type="button"
          role="radio"
          aria-checked={value === mode}
          disabled={disabled}
          onClick={() => onChange(mode)}
          className={cn(
            'cursor-pointer rounded-full px-3 py-1 text-sm text-text-3 transition-colors disabled:cursor-not-allowed',
            value === mode && 'bg-night-2 text-text',
          )}
        >
          {t(`requestModes.${mode}`)}
        </button>
      ))}
    </div>
  )
}

/** Shown while no category is switched on (ADM-14): such a member can sign in but not request. */
export function NoCategoriesWarning({ entitlements }: { entitlements: Entitlement[] }) {
  const { t } = useTranslation('admin')
  if (entitlements.some((e) => e.enabled)) return null
  return (
    <p
      role="note"
      className="flex items-start gap-2.5 rounded-card border border-line bg-night-1 px-3 py-2.5 text-sm text-text-2"
    >
      <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-warn" />
      {t('members.noCategoriesWarning')}
    </p>
  )
}

/** A one-time link with a copy button. */
export function CopyLink({ url, label }: { url: string; label: string }) {
  const { t } = useTranslation()
  const id = useId()
  const [copied, setCopied] = useState(false)
  const link = absoluteUrl(url)
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(link)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2000)
    } catch {
      // Clipboard blocked (http, permissions): the field stays selectable.
      document.getElementById(id)?.focus()
    }
  }
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm font-medium text-text-2">
        {label}
      </label>
      <div className="flex gap-2">
        <input
          id={id}
          readOnly
          value={link}
          onFocus={(e) => e.currentTarget.select()}
          className="type-data h-10 min-w-0 flex-1 rounded-control border border-line bg-night-0 px-3 text-sm text-text"
        />
        <Button type="button" variant="primary" onClick={() => void copy()}>
          {copied ? <CheckIcon aria-hidden /> : <CopyIcon aria-hidden />}
          {copied ? t('actions.copied') : t('actions.copy')}
        </Button>
      </div>
    </div>
  )
}

/**
 * Inline error with icon, for failed submits and loads. `focus`: takes focus when it appears, so
 * a failed form submit never leaves focus on the button or `<body>` (spec 0008 F3, AUTH-04).
 */
export function ErrorNotice({ children, focus }: { children: ReactNode; focus?: boolean }) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (focus) ref.current?.focus()
  }, [focus])
  useScrollIntoDialogView(ref)
  return (
    <div
      ref={ref}
      role="alert"
      tabIndex={focus ? -1 : undefined}
      className="chip flex items-start gap-2.5 rounded-card px-4 py-3 text-base [--chip:var(--color-bad)]"
    >
      <CircleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-bad" />
      <span>{children}</span>
    </div>
  )
}
