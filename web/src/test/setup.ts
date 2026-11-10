import { cleanup } from '@testing-library/react'
import { afterEach } from 'vitest'

import '@/i18n'

afterEach(cleanup)

// jsdom has no ResizeObserver; Radix measures switches and checkboxes inside forms with it.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
}
