import { lazy } from 'react'

/*
 * The private pages render only inside an open area (`PrivateLayout`, under its Suspense), so
 * their chunk is fetched only once the area is open: a closed or absent area requests nothing a
 * plain "not found" would not (PRIV-03).
 */
const privatePages = () => import('@/pages/member/PrivatePages')
const privateRequests = () => import('@/pages/member/PrivateRequestsPage')
export const PrivateHomePage = lazy(() =>
  privatePages().then((m) => ({ default: m.PrivateHomePage })),
)
export const PrivateSearchPage = lazy(() =>
  privatePages().then((m) => ({ default: m.PrivateSearchPage })),
)
export const PrivateScenePage = lazy(() =>
  privatePages().then((m) => ({ default: m.PrivateScenePage })),
)
export const PrivateEntityPage = lazy(() =>
  privatePages().then((m) => ({ default: m.PrivateEntityPage })),
)
export const PrivateRequestsPage = lazy(() =>
  privateRequests().then((m) => ({ default: m.PrivateRequestsPage })),
)
export const PrivateFollowingPage = lazy(() =>
  privateRequests().then((m) => ({ default: m.PrivateFollowingPage })),
)
