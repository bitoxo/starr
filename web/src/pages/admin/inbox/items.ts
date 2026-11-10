import {
  AlertTriangleIcon,
  CircleSlashIcon,
  ContactIcon,
  DiscIcon,
  HourglassIcon,
  KeyRoundIcon,
  LibraryIcon,
  ListChecksIcon,
  type LucideIcon,
  OctagonXIcon,
  PauseIcon,
  ShieldAlertIcon,
  Undo2Icon,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'

import type { AttentionItem, AttentionKind } from '@/api/attention'
import { type CategoryName, seasonLabel } from '@/api/requests'
import type { StatusTone } from '@/components/StatusChip'

/** Icon and tone of each attention kind (plain chips, ADM-38). */
export const KIND: Record<AttentionKind, { icon: LucideIcon; tone: StatusTone }> = {
  approval: { icon: HourglassIcon, tone: 'info' },
  failed: { icon: OctagonXIcon, tone: 'bad' },
  stuck: { icon: AlertTriangleIcon, tone: 'warn' },
  category_closed: { icon: CircleSlashIcon, tone: 'warn' },
  account_conflict: { icon: ShieldAlertIcon, tone: 'warn' },
  account_failed: { icon: AlertTriangleIcon, tone: 'bad' },
  account_deviation: { icon: Undo2Icon, tone: 'info' },
  account_without_password: { icon: KeyRoundIcon, tone: 'warn' },
  library_unmapped: { icon: LibraryIcon, tone: 'warn' },
  library_stale: { icon: LibraryIcon, tone: 'warn' },
  album_unlisted: { icon: DiscIcon, tone: 'warn' },
  writes_paused: { icon: PauseIcon, tone: 'warn' },
  held: { icon: PauseIcon, tone: 'warn' },
  account_plan_held: { icon: ContactIcon, tone: 'warn' },
  setup: { icon: ListChecksIcon, tone: 'neutral' },
}

/** The category's name in the admin area: "Private" for the adult category (PRIV-15). */
export function useCategoryLabel() {
  const { t } = useTranslation(['admin', 'common'])
  return (category: CategoryName) =>
    category === 'adult' ? t('requests.filters.private') : t(`common:categories.${category}`)
}

/** "Severance S03", "Radiohead – OK Computer", or the neutral line of a private item. */
function itemTitle(item: AttentionItem, t: (key: string, o?: object) => string): string {
  const { request, acquisition } = item
  if (request) return request.title
  if (acquisition) {
    const unit = acquisition.album?.title ?? seasonLabel([acquisition.season])
    return unit && !acquisition.redacted
      ? acquisition.album
        ? `${acquisition.title} – ${unit}`
        : `${acquisition.title} ${unit}`
      : acquisition.title
  }
  const w = item.writes
  switch (item.kind) {
    case 'category_closed':
      return t('inbox.closed.title', { category: item.category?.category ?? '' })
    case 'account_conflict':
      return t('inbox.conflict.title', {
        service: item.conflict?.serviceName,
        username: item.conflict?.username,
      })
    case 'account_failed':
      return t('inbox.accountFailed.title', {
        service: item.accountFailure?.serviceName,
        username: item.accountFailure?.username,
      })
    case 'account_deviation':
      return t('inbox.deviation.title', {
        service: item.deviation?.serviceName,
        username: item.deviation?.username,
      })
    case 'account_without_password':
      return t('inbox.withoutPassword.title', {
        service: item.withoutPassword?.serviceName,
        username: item.withoutPassword?.username,
      })
    case 'library_unmapped':
      return item.library?.name ?? ''
    case 'library_stale':
      return t('inbox.stale.title', {
        category: item.staleLibrary?.category,
        service: item.staleLibrary?.serviceName,
      })
    case 'album_unlisted':
      return t('inbox.album.title', { artist: item.album?.artist, album: item.album?.album })
    case 'held':
    case 'writes_paused':
      return w?.kind === 'accounts'
        ? t('inbox.writes.accountsTitle', { service: w.serviceName })
        : t('inbox.writes.sourceTitle', { service: w?.serviceName })
    case 'account_plan_held':
      return t('inbox.plan.title', { service: item.plan?.serviceName })
    case 'setup':
      return t('inbox.setup.title')
    default:
      return ''
  }
}

/** The title as a translated line, with category names spelled out. */
export function useItemTitle() {
  const { t } = useTranslation('admin')
  const category = useCategoryLabel()
  return (item: AttentionItem) => {
    if (item.kind === 'category_closed' && item.category) {
      return t('inbox.closed.title', { category: category(item.category.category) })
    }
    if (item.kind === 'library_stale' && item.staleLibrary) {
      return t('inbox.stale.title', {
        category: category(item.staleLibrary.category),
        service: item.staleLibrary.serviceName,
      })
    }
    return itemTitle(item, t as (key: string, o?: object) => string)
  }
}
