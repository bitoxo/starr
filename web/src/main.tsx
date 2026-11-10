import { QueryClientProvider } from '@tanstack/react-query'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { RouterProvider } from 'react-router'
import { z } from 'zod'

import { queryClient } from '@/api/queryClient'
import { Toaster } from '@/components/ui/sonner'
import { TooltipProvider } from '@/components/ui/tooltip'
import { router } from '@/router'

import './i18n'
import './index.css'

// The Content-Security-Policy forbids eval: zod's JIT would probe `new Function` on the first
// parse and log a CSP violation. Plain interpretation is fast enough for Starr's forms.
z.config({ jitless: true })

// After an update a stale tab asks for chunks of the old build, which are gone: reload once
// (the flag stops a loop and clears itself once the new build has run for a while).
const RELOADED = 'starr.chunkReload'
window.addEventListener('vite:preloadError', (event) => {
  try {
    if (sessionStorage.getItem(RELOADED)) return
    sessionStorage.setItem(RELOADED, '1')
  } catch {
    return
  }
  event.preventDefault()
  window.location.reload()
})
window.setTimeout(() => {
  try {
    sessionStorage.removeItem(RELOADED)
  } catch {
    // Storage blocked: nothing to clear.
  }
}, 10_000)

const root = document.getElementById('root')
if (!root) throw new Error('#root element missing from index.html')

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <RouterProvider router={router} />
        <Toaster />
      </TooltipProvider>
    </QueryClientProvider>
  </StrictMode>,
)
