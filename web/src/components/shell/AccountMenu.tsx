import {
  ArrowLeftRightIcon,
  CheckIcon,
  ChevronsUpDownIcon,
  EyeIcon,
  EyeOffIcon,
  KeyboardIcon,
  LogOutIcon,
  ShieldIcon,
  UserIcon,
} from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate } from 'react-router'

import { useSetPrivateUnlock } from '@/api/privacy'
import { switchAccount } from '@/api/security'
import { signOut, useSession } from '@/auth/session'
import { LazyBackToAdminDialog } from '@/components/LazyBackToAdminDialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { formatTime } from '@/lib/format'
import { cn } from '@/lib/utils'

import { setShortcutsEnabled, useShortcutsEnabled } from './prefs'
import { RailTip } from './Sidebar'

export type ShellVariant = 'member' | 'admin'

/**
 * The account button and its menu: member view switch, the private unlock switch (only for
 * accounts that may unlock it), Profile, sign-out. `row` in the expanded sidebar, `rail` as the
 * avatar alone, `icon` in the mobile top bar.
 */
export function AccountMenu({
  variant,
  layout,
}: {
  variant: ShellVariant
  layout: 'row' | 'rail' | 'icon'
}) {
  const { t } = useTranslation()
  const session = useSession()
  const navigate = useNavigate()
  const name = session.displayName ?? ''
  const linked = variant === 'admin' ? session.me?.linkedMember : null
  const switchedFrom = variant === 'member' ? session.me?.switchedFrom : null
  const [backToAdmin, setBackToAdmin] = useState(false)
  // Only accounts that may unlock the private category get it (spec 0007 Q7).
  const privateCategory = session.me?.privateCategory
  const setPrivate = useSetPrivateUnlock()
  // Members switch shortcuts under Profile → Preferences; admins have no profile page.
  const shortcuts = useShortcutsEnabled()

  const avatar = (
    <span
      aria-hidden
      className="grid size-7 shrink-0 place-items-center rounded-full border border-line bg-night-2 text-sm font-semibold text-text uppercase"
    >
      {name.slice(0, 1)}
    </span>
  )

  return (
    <>
      <DropdownMenu>
        <RailTip rail={layout === 'rail'} label={name}>
          <DropdownMenuTrigger
            className={cn(
              'flex shrink-0 cursor-pointer items-center gap-3 text-left text-text-2 transition-colors hover:bg-night-2/60 hover:text-text data-[state=open]:bg-night-2 [&_svg]:size-4',
              layout === 'row' && 'h-12 w-full rounded-card px-3',
              layout === 'rail' && 'h-12 w-full justify-center rounded-card',
              layout === 'icon' && 'size-11 justify-center rounded-full',
            )}
          >
            {avatar}
            {layout === 'row' ? (
              <>
                <span className="flex min-w-0 flex-col leading-tight">
                  <span className="truncate text-base font-medium text-text">{name}</span>
                  <span className="text-xs text-text-3">
                    {variant === 'admin' ? t('account.roleAdmin') : t('account.roleMember')}
                  </span>
                </span>
                <ChevronsUpDownIcon aria-hidden className="ml-auto text-text-3" />
              </>
            ) : (
              <span className="sr-only">{name}</span>
            )}
          </DropdownMenuTrigger>
        </RailTip>
        <DropdownMenuContent
          side={layout === 'icon' ? 'bottom' : 'top'}
          align={layout === 'icon' ? 'end' : 'start'}
          className={layout === 'row' ? 'w-(--radix-dropdown-menu-trigger-width)' : 'w-64'}
        >
          <DropdownMenuLabel>
            {switchedFrom
              ? t('account.memberViewOf', { name: switchedFrom.displayName })
              : t('account.signedInAs', { name })}
          </DropdownMenuLabel>
          {linked?.active && (
            <DropdownMenuItem
              onSelect={() => {
                void switchAccount()
                  .then(() => navigate('/', { replace: true }))
                  .catch(() => undefined)
              }}
            >
              <ArrowLeftRightIcon aria-hidden />
              {t('account.switchToMember')}
            </DropdownMenuItem>
          )}
          {switchedFrom && (
            <DropdownMenuItem onSelect={() => setBackToAdmin(true)}>
              <ShieldIcon aria-hidden />
              {t('account.backToAdmin')}
            </DropdownMenuItem>
          )}
          {privateCategory && (
            <DropdownMenuItem
              disabled={setPrivate.isPending}
              // Locking leaves the private area at once (the gate watcher, spec 0008 F2).
              onSelect={() => setPrivate.mutate(!privateCategory.unlocked)}
              className="items-start"
            >
              {privateCategory.unlocked ? (
                <EyeOffIcon aria-hidden className="mt-0.5" />
              ) : (
                <EyeIcon aria-hidden className="mt-0.5" />
              )}
              <span className="flex flex-col">
                {privateCategory.unlocked ? t('account.privateHide') : t('account.privateShow')}
                <span className="text-xs text-text-3">
                  {privateCategory.unlocked && privateCategory.unlockedUntil
                    ? t('account.privateHintOpen', {
                        time: formatTime(privateCategory.unlockedUntil),
                      })
                    : t('account.privateHintLocked')}
                </span>
              </span>
            </DropdownMenuItem>
          )}
          {variant === 'member' && (
            <DropdownMenuItem asChild>
              <Link to="/profile">
                <UserIcon aria-hidden />
                {t('nav.profile')}
              </Link>
            </DropdownMenuItem>
          )}
          {variant === 'admin' && (
            <DropdownMenuItem
              role="menuitemcheckbox"
              aria-checked={shortcuts}
              // Stays open, so the new state can be seen.
              onSelect={(event) => {
                event.preventDefault()
                setShortcutsEnabled(!shortcuts)
              }}
              className="items-start"
            >
              <KeyboardIcon aria-hidden className="mt-0.5" />
              <span className="flex flex-1 flex-col">
                {t('account.shortcuts')}
                <span className="text-xs text-text-3">{t('account.shortcutsHint')}</span>
              </span>
              <CheckIcon aria-hidden className={cn('mt-0.5', !shortcuts && 'invisible')} />
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            onSelect={() => {
              void signOut().finally(() => navigate('/login', { replace: true }))
            }}
          >
            <LogOutIcon aria-hidden />
            {t('account.signOut')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {switchedFrom && (
        <LazyBackToAdminDialog
          open={backToAdmin}
          onOpenChange={setBackToAdmin}
          adminName={switchedFrom.displayName}
          totpEnabled={switchedFrom.totpEnabled}
        />
      )}
    </>
  )
}
