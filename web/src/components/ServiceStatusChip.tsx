import { useTranslation } from 'react-i18next'

import type { Service } from '@/api/services'
import { StatusChip, type StatusTone } from '@/components/StatusChip'
import { type ServiceStatus, serviceStatus } from '@/lib/services'

const TONE: Record<ServiceStatus, StatusTone> = {
  pending: 'info',
  ok: 'ok',
  warning: 'warn',
  error: 'bad',
}

export function ServiceStatusChip({ service }: { service: Service }) {
  const { t } = useTranslation('admin')
  const status = serviceStatus(service)
  return <StatusChip tone={TONE[status]}>{t(`services.status.${status}`)}</StatusChip>
}
