import { useTranslation } from 'react-i18next'

import { StatusChip } from '@/components/StatusChip'
import { STATES, stateLabel, type StateView } from '@/lib/states'

/**
 * A request's state as a chip: label, tone and icon all come from the one state table
 * (`lib/states.ts`, spec 0008 F1), so the same state reads the same everywhere.
 */
export function StateChip({ state, className }: { state: StateView; className?: string }) {
  // Re-renders when the language changes; the label itself comes from the table.
  useTranslation()
  const entry = STATES[state.key]
  return (
    <StatusChip tone={entry.tone} icon={entry.icon} outline={entry.outline} className={className}>
      {stateLabel(state)}
    </StatusChip>
  )
}
