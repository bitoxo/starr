import { BellIcon, MailIcon, MailXIcon } from 'lucide-react'
import { useId } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { useApiErrorMessage } from '@/api/errors'
import {
  MEMBER_KINDS,
  type MemberKind,
  useNotificationPreferences,
  useUpdateNotificationPreferences,
} from '@/api/notifications'
import { ErrorNotice } from '@/components/members'
import { Switch } from '@/components/ui/switch'
import { toggleKind } from '@/lib/notifications'

/** The member kinds of a list, in display order (the server only offers and stores those). */
function memberKinds(kinds: string[]): MemberKind[] {
  return MEMBER_KINDS.filter((k) => kinds.includes(k))
}

/** Which follows `follow_requested` is about: the member's series, music, or both. */
type FollowScope = 'both' | 'series' | 'music' | 'other'

function followScope(follows: string[]): FollowScope {
  const series = follows.includes('series')
  const music = follows.includes('music')
  if (series && music) return 'both'
  if (series) return 'series'
  if (music) return 'music'
  return 'other'
}

/**
 * In-app is always on; email per kind is the member's opt-in. Only the kinds the server offers
 * are listed (spec 0008 PRV-01): those of the member's categories and, for the private
 * category, only while the session has unlocked it. Without email (no mail server, no address)
 * there are no dead switches: a notice says why and who can change it, and each kind says it
 * comes to the bell (MEM-28).
 */
export function NotificationPreferences() {
  const { t } = useTranslation('member')
  const prefs = useNotificationPreferences()
  const update = useUpdateNotificationPreferences()
  const errorMessage = useApiErrorMessage()

  if (prefs.isError) return <ErrorNotice>{errorMessage(prefs.error)}</ErrorNotice>
  if (!prefs.data) return null
  const { email, emailAvailable, emailAddress } = prefs.data
  const canEmail = emailAvailable && emailAddress != null
  const offered = memberKinds(prefs.data.kinds)
  // While the private category is open (its weekly cap is offered), follows also cover
  // performers and studios: the wording then names no kind of title.
  const scope = offered.includes('follow_capped') ? 'other' : followScope(prefs.data.follows)

  const toggle = (kind: MemberKind) =>
    // The switch itself is the confirmation; only failures speak up.
    update.mutate(toggleKind(memberKinds(email), kind, offered), {
      onError: (error) => toast.error(errorMessage(error)),
    })

  return (
    <div className="flex flex-col gap-4">
      <p className="flex items-start gap-2 text-sm text-text-2">
        <BellIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-text-3" />
        {t('notifications.inAppAlways')}
      </p>
      {canEmail ? (
        <p className="flex items-start gap-2 text-sm text-text-2">
          <MailIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-text-3" />
          {t('notifications.emailTo', { address: emailAddress })}
        </p>
      ) : (
        <div className="flex items-start gap-3 rounded-card border border-line bg-night-1 px-4 py-3">
          <MailXIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-text-3" />
          <div className="flex flex-col gap-1 text-sm">
            <p className="font-medium text-text">
              {!emailAvailable ? t('notifications.emailUnavailable') : t('notifications.noAddress')}
            </p>
            <p className="text-text-2">
              {!emailAvailable
                ? t('notifications.emailUnavailableNext')
                : t('notifications.noAddressNext')}
            </p>
          </div>
        </div>
      )}
      <ul
        aria-label={canEmail ? t('notifications.emailListLabel') : t('notifications.bellListLabel')}
        className="flex flex-col divide-y divide-line rounded-card border border-line"
      >
        {offered.map((kind) => (
          <KindRow
            key={kind}
            kind={kind}
            scope={scope}
            checked={memberKinds(email).includes(kind)}
            canEmail={canEmail}
            disabled={update.isPending}
            onToggle={() => toggle(kind)}
          />
        ))}
      </ul>
    </div>
  )
}

function KindRow({
  kind,
  scope,
  checked,
  canEmail,
  disabled,
  onToggle,
}: {
  kind: MemberKind
  scope: FollowScope
  checked: boolean
  canEmail: boolean
  disabled: boolean
  onToggle: () => void
}) {
  const { t } = useTranslation('member')
  const id = useId()
  // "New season or album you follow" names only what the member can follow.
  const label =
    kind === 'follow_requested' && scope !== 'both'
      ? t(`notifications.followRequested.${scope}.label`)
      : t(`notifications.kinds.${kind}`)
  const hint =
    kind === 'follow_requested' && scope !== 'both'
      ? t(`notifications.followRequested.${scope}.hint`)
      : t(`notifications.kindHints.${kind}`)
  if (!canEmail) {
    return (
      <li className="flex items-center gap-4 px-4 py-3">
        <div className="flex min-w-0 flex-1 flex-col">
          <span className="text-base text-text">{label}</span>
          <span className="text-sm text-text-3">{hint}</span>
        </div>
        <span className="flex shrink-0 items-center gap-1.5 text-sm text-text-2">
          <BellIcon aria-hidden className="size-3.5 text-text-3" />
          {t('notifications.inBell')}
        </span>
      </li>
    )
  }
  return (
    <li className="flex items-center gap-4 px-4 py-3">
      <label htmlFor={id} className="flex min-w-0 flex-1 cursor-pointer flex-col">
        <span className="text-base text-text">{label}</span>
        <span className="text-sm text-text-3">{hint}</span>
      </label>
      <Switch
        id={id}
        checked={checked}
        disabled={disabled}
        onCheckedChange={onToggle}
        aria-label={t('notifications.emailFor', { kind: label })}
      />
    </li>
  )
}
