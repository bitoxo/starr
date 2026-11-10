import { expect } from 'vitest'

/** After any close path focus must land somewhere useful, never on `<body>` (spec 0008 F3). */
export function expectFocusNotOnBody() {
  expect(document.activeElement, 'focus fell to <body>').not.toBe(document.body)
  expect(document.activeElement).not.toBeNull()
}
