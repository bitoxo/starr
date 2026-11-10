import 'i18next'

import type { resources } from './locales'

// Typed keys: `t('missing.key')` is a compile error.
declare module 'i18next' {
  interface CustomTypeOptions {
    defaultNS: 'common'
    resources: (typeof resources)['en']
  }
}
