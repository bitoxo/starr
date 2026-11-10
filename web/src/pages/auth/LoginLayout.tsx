import { useQuery } from '@tanstack/react-query'
import type { ComponentProps } from 'react'

import { api } from '@/api/client'
import { unwrap } from '@/api/errors'
import { AuthLayout } from '@/components/AuthLayout'

/** Cells of the poster wall; fewer posters repeat to fill it. */
const CELLS = 16
/** Below this the wall would look like a gap, so it is left out. */
const MIN_POSTERS = 4

/** The URL of one login poster (served only while it is in the list). */
function loginPosterUrl(file: string): string {
  return `/api/v1/login-art/${encodeURIComponent(file)}`
}

/**
 * The public frame with poster art beside the content (spec 0008 slice 11), for the sign-in
 * pages: posters of the cached movie and series Trending lists from `GET /api/v1/login-art`,
 * never anything of the private category (the server only ever reads those two categories).
 * Decorative: `aria-hidden`, no text on top of it, shown from 1024 px only, masked into the
 * ground at its inner edge and dimmed by a fixed cap (`.login-art`). Static, so there is nothing
 * to reduce for reduced motion. While the list is empty or failed it is the plain frame.
 */
export function LoginLayout(props: Omit<ComponentProps<typeof AuthLayout>, 'art'>) {
  const art = useQuery({
    queryKey: ['login-art'],
    queryFn: () => unwrap(api.GET('/api/v1/login-art')),
    staleTime: 5 * 60_000,
    retry: false,
  })
  const posters = art.data?.posters ?? []
  return (
    <AuthLayout
      {...props}
      art={posters.length >= MIN_POSTERS ? <PosterWall posters={posters} /> : undefined}
    />
  )
}

function PosterWall({ posters }: { posters: string[] }) {
  const cells = Array.from({ length: CELLS }, (_, i) => posters[i % posters.length])
  return (
    <div
      aria-hidden
      data-login-art
      className="login-art pointer-events-none absolute inset-y-0 right-0 hidden w-1/2 overflow-hidden lg:block"
    >
      <div className="grid -rotate-6 scale-125 grid-cols-4 gap-3 pt-6">
        {cells.map((file, i) => (
          <img
            // A poster may repeat; its place in the wall keeps it apart.
            key={`${file}-${i}`}
            src={loginPosterUrl(file)}
            alt=""
            decoding="async"
            draggable={false}
            onError={(e) => {
              e.currentTarget.style.visibility = 'hidden'
            }}
            className="aspect-[2/3] w-full rounded-card bg-night-1 object-cover"
          />
        ))}
      </div>
    </div>
  )
}
