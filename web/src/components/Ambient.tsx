import { useState } from 'react'

import { cn } from '@/lib/utils'

/**
 * The ambient layer (design README A1): the current title's backdrop, blurred, saturated and
 * dimmed, behind the page as a non-interactive layer. It cross-fades with `--dur-ambient` when
 * the title changes (instantly under reduced motion); its opacity is capped in CSS
 * (`.ambient-image`) so every text token keeps 4.5:1 over any image. Without an image the page
 * stays plain `night-0`. `src` must be a proxied URL (`/api/v1/images/…`).
 */
export function Ambient({ src, className }: { src: string | null; className?: string }) {
  // The current backdrop and the one before it, enough for the cross-fade.
  const [seen, setSeen] = useState<string[]>([])
  if (src && seen[seen.length - 1] !== src)
    setSeen([...seen.filter((u) => u !== src).slice(-1), src])
  return (
    <div
      aria-hidden
      data-ambient
      className={cn('pointer-events-none absolute -z-10 overflow-hidden', className)}
    >
      {seen.map((url) => (
        <img
          key={url}
          src={url}
          alt=""
          decoding="async"
          data-on={url === src ? '' : undefined}
          className="ambient-image"
        />
      ))}
    </div>
  )
}
