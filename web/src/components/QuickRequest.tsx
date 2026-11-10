import { PlusIcon } from 'lucide-react'
import { type ReactNode, type RefObject, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { type TitleCard, useCreateRequest } from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { StateChip } from '@/components/StateChip'
import { Button } from '@/components/ui/button'
import { cardState, type StateView } from '@/lib/states'

/**
 * The one action of a focused title (hero, preview card, info strip; spec 0008 A3, A4): its
 * state chip once it has one, else Request in Polaris (one gold per focused context). Movies
 * and albums are requested at once; a series opens its detail view to choose seasons. Nothing
 * while the category takes no requests. The request is a member's own and never private here:
 * Discover and search show no private titles.
 */
export function QuickRequest({
  card,
  requestable,
  onOpen,
  size = 'default',
  focusAfter,
  inert = false,
  renderState,
  className,
}: {
  card: TitleCard
  requestable: boolean
  onOpen: (card: TitleCard) => void
  size?: 'default' | 'sm'
  /** Where focus goes when the button gives way to the chip (never `<body>`, spec 0008 F3). */
  focusAfter?: RefObject<HTMLElement | null>
  /** Out of the tab order (the preview card is for pointers only). */
  inert?: boolean
  /** Shows the state instead of the chip (the hero's status plaque). */
  renderState?: (state: StateView) => ReactNode
  className?: string
}) {
  const { t } = useTranslation('member')
  const create = useCreateRequest()
  const errorMessage = useApiErrorMessage()
  // Until the lists come back with the new state, show what the server just said.
  const [done, setDone] = useState<StateView | null>(null)
  const state = cardState(card) ?? done

  if (state && renderState) return renderState(state)
  if (state) return <StateChip state={state} className={size === 'sm' ? 'text-xs' : undefined} />
  if (!requestable || card.kind === 'artist') return null
  if (card.kind === 'series') {
    return (
      <Button
        variant="primary"
        size={size}
        className={className}
        tabIndex={inert ? -1 : undefined}
        onClick={() => onOpen(card)}
      >
        <PlusIcon aria-hidden />
        {t('discover.card.chooseSeasons')}
      </Button>
    )
  }
  return (
    <Button
      variant="primary"
      size={size}
      className={className}
      tabIndex={inert ? -1 : undefined}
      // Not `disabled` while it runs: that would drop the focus to <body>.
      aria-disabled={create.isPending || undefined}
      onClick={(event) => {
        if (create.isPending) return
        const hadFocus = event.currentTarget === document.activeElement
        create.mutate(
          { kind: card.kind, titleId: card.id },
          {
            onSuccess: ({ request }) => {
              const pending = request.state === 'pending_approval'
              setDone({ key: pending ? 'awaitingApproval' : 'approved' })
              toast(
                t(pending ? 'discover.card.requestedPending' : 'discover.card.requestedApproved', {
                  title: card.title,
                }),
              )
              if (hadFocus) focusAfter?.current?.focus()
            },
            onError: (error) => {
              toast.error(errorMessage(error))
            },
          },
        )
      }}
    >
      <PlusIcon aria-hidden />
      {t('discover.card.request')}
    </Button>
  )
}
