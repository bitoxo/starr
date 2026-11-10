import type { ComponentType } from 'react'
import { createBrowserRouter, Navigate, Outlet, type RouteObject } from 'react-router'

import { GuestOnly, RequireRole } from '@/auth/guards'
import { AppShell } from '@/components/AppShell'
import { LegacyRedirect } from '@/components/shell/LegacyRedirect'
import { BootSkeleton } from '@/components/shell/ShellSkeleton'
import { ErrorPage, NotFound } from '@/pages/ErrorPage'
import DiscoverPage from '@/pages/member/DiscoverPage'
import { PrivateLayout } from '@/pages/member/PrivateLayout'
import {
  PrivateEntityPage,
  PrivateFollowingPage,
  PrivateHomePage,
  PrivateRequestsPage,
  PrivateScenePage,
  PrivateSearchPage,
} from '@/pages/member/privateRoutes'

/*
 * Code splitting (spec 0008 slice 12): the entry chunk holds the shell and Discover;
 * every other page is its own chunk, loaded on the first visit (the page shown so far stays
 * until it is there; on a cold start the boot skeleton shows).
 */
const page = (load: () => Promise<{ default: ComponentType }>) => async () => ({
  Component: (await load()).default,
})
const named =
  <M,>(load: () => Promise<M>, pick: (m: M) => ComponentType) =>
  async () => ({ Component: pick(await load()) })

const profile = () => import('@/pages/member/ProfilePage')
const passwords = () => import('@/pages/auth/PasswordPages')

/** An old admin URL (spec 0008 Q22): redirected with `replace`, query parameters kept. */
const moved = (path: string): RouteObject => ({
  path,
  element: (
    <LegacyRedirect>
      <NotFound />
    </LegacyRedirect>
  ),
})

export const routes: RouteObject[] = [
  {
    errorElement: <ErrorPage />,
    hydrateFallbackElement: <BootSkeleton />,
    children: [
      {
        element: <GuestOnly />,
        children: [
          { path: '/setup', lazy: page(() => import('@/pages/auth/SetupPage')) },
          { path: '/login', lazy: page(() => import('@/pages/auth/LoginPage')) },
          {
            path: '/forgot-password',
            lazy: page(() => import('@/pages/auth/ForgotPasswordPage')),
          },
        ],
      },
      // Token links work regardless of the current session.
      { path: '/invite/:token', lazy: named(passwords, (m) => m.AcceptInvitePage) },
      { path: '/set-password/:token', lazy: named(passwords, (m) => m.SetPasswordPage) },
      {
        element: <RequireRole role="member" />,
        children: [
          {
            element: <AppShell variant="member" />,
            children: [
              { path: '/', element: <DiscoverPage /> },
              {
                path: '/discover/:category',
                lazy: page(() => import('@/pages/member/CategoryPage')),
              },
              {
                path: '/discover/:category/:list',
                lazy: page(() => import('@/pages/member/SeeAllPage')),
              },
              { path: '/search', lazy: page(() => import('@/pages/member/SearchPage')) },
              { path: '/requests', lazy: page(() => import('@/pages/member/RequestsPage')) },
              { path: '/services', lazy: page(() => import('@/pages/member/MyServicesPage')) },
              {
                path: '/profile',
                lazy: page(profile),
                children: [
                  { index: true, lazy: named(profile, (m) => m.ProfileAccount) },
                  { path: 'notifications', lazy: named(profile, (m) => m.ProfileNotifications) },
                  { path: 'sessions', lazy: named(profile, (m) => m.ProfileSessions) },
                  { path: 'preferences', lazy: named(profile, (m) => m.ProfilePreferences) },
                ],
              },
              {
                // The private category's area (spec 0007): "not found" while the gate is closed.
                path: '/private',
                element: <PrivateLayout />,
                children: [
                  { index: true, element: <PrivateHomePage /> },
                  { path: 'search', element: <PrivateSearchPage /> },
                  { path: 'requests', element: <PrivateRequestsPage /> },
                  { path: 'following', element: <PrivateFollowingPage /> },
                  { path: 'scenes/:id', element: <PrivateScenePage /> },
                  { path: 'performers/:id', element: <PrivateEntityPage kind="performer" /> },
                  { path: 'studios/:id', element: <PrivateEntityPage kind="studio" /> },
                ],
              },
              // Not found inside the shell, the same markup as a closed private area.
              { path: '*', element: <NotFound /> },
            ],
          },
        ],
      },
      {
        path: '/admin',
        element: <RequireRole role="admin" />,
        children: [
          {
            element: <AppShell variant="admin" />,
            children: [
              // The admin lands on the Inbox (spec 0008 ADM-22, Q22).
              { index: true, element: <Navigate to="/admin/inbox" replace /> },
              { path: 'inbox', lazy: page(() => import('@/pages/admin/InboxPage')) },
              {
                path: 'requests',
                // `?view=acquisitions` was the Downloads view before it got its own route.
                element: (
                  <LegacyRedirect>
                    <Outlet />
                  </LegacyRedirect>
                ),
                children: [{ index: true, lazy: page(() => import('@/pages/admin/RequestsPage')) }],
              },
              { path: 'downloads', lazy: page(() => import('@/pages/admin/RequestsPage')) },
              {
                path: 'people',
                children: [
                  { index: true, element: <Navigate to="/admin/people/members" replace /> },
                  { path: 'members', lazy: page(() => import('@/pages/admin/MembersPage')) },
                  {
                    path: 'members/:id',
                    lazy: page(() => import('@/pages/admin/MemberDetailPage')),
                  },
                  {
                    path: 'invitations',
                    lazy: page(() => import('@/pages/admin/InvitationsPage')),
                  },
                  { path: 'admins', lazy: page(() => import('@/pages/admin/AdminsPage')) },
                ],
              },
              {
                path: 'media',
                children: [
                  { index: true, element: <Navigate to="/admin/media/services" replace /> },
                  { path: 'services', lazy: page(() => import('@/pages/admin/ServicesPage')) },
                  {
                    path: 'categories',
                    lazy: page(() => import('@/pages/admin/CategoriesPage')),
                  },
                  {
                    path: 'categories/:category',
                    lazy: page(() => import('@/pages/admin/CategoriesPage')),
                  },
                  { path: 'accounts', lazy: page(() => import('@/pages/admin/AccountsPage')) },
                ],
              },
              {
                path: 'system',
                children: [
                  { index: true, element: <Navigate to="/admin/system/runs" replace /> },
                  { path: 'runs', lazy: page(() => import('@/pages/admin/RunsPage')) },
                  {
                    path: 'notifications',
                    lazy: page(() => import('@/pages/admin/NotificationsSettingsPage')),
                  },
                  { path: 'security', lazy: page(() => import('@/pages/admin/SecurityPage')) },
                  { path: 'about', lazy: page(() => import('@/pages/admin/SettingsPage')) },
                ],
              },
              // Old routes, kept working for the 1.x line (ADR 0015).
              moved('needs-you'),
              moved('members'),
              moved('members/:id'),
              moved('invitations'),
              moved('services'),
              moved('accounts'),
              moved('categories'),
              moved('runs'),
              moved('settings'),
              moved('settings/security'),
              moved('settings/admins'),
              moved('settings/notifications'),
              { path: '*', element: <NotFound /> },
            ],
          },
        ],
      },
    ],
  },
]

export const router = createBrowserRouter(routes)
