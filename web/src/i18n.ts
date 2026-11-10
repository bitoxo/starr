import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'

import { namespaces, resources } from './locales'

void i18n.use(initReactI18next).init({
  lng: 'en',
  fallbackLng: 'en',
  defaultNS: 'common',
  ns: namespaces,
  resources,
  interpolation: { escapeValue: false },
})

export default i18n
