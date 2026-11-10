import { type ComponentProps, lazy, Suspense } from 'react'

import type { BackToAdminDialog as Dialog } from '@/components/BackToAdminDialog'

// Only admins in the member view ever see it: its form code stays out of the entry chunk.
const BackToAdminDialog = lazy(() =>
  import('@/components/BackToAdminDialog').then((m) => ({ default: m.BackToAdminDialog })),
)

/** `BackToAdminDialog`, loaded the first time it renders (spec 0008 slice 12). */
export function LazyBackToAdminDialog(props: ComponentProps<typeof Dialog>) {
  return (
    <Suspense fallback={null}>
      <BackToAdminDialog {...props} />
    </Suspense>
  )
}
