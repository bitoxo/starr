import type { ReactNode } from 'react'

import { InPrivateArea } from '@/lib/privateArea'

/**
 * Marks the private (adult) area (spec 0007): only inside it may covers of the private
 * category render. Outside, `Cover` refuses them: no image, not even blurred
 * (`docs/design`: "never adult covers outside unlocked areas").
 */
export function PrivateArea({ children }: { children: ReactNode }) {
  return <InPrivateArea.Provider value>{children}</InPrivateArea.Provider>
}
