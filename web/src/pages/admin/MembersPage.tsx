import { zodResolver } from '@hookform/resolvers/zod'
import { UserPlusIcon, UsersIcon } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { z } from 'zod'

import { useApiErrorMessage } from '@/api/errors'
import {
  defaultEntitlements,
  useCreateMember,
  useMembers,
  type Entitlement,
  type Link as OneTimeLink,
  type Member,
  type MemberStatus,
} from '@/api/members'
import { PageHeader } from '@/components/AppShell'
import { type Column, DataTable } from '@/components/DataTable'
import { EmptyState } from '@/components/EmptyState'
import { FormField } from '@/components/FormField'
import {
  CategoryList,
  CopyLink,
  EntitlementsEditor,
  ErrorNotice,
  MemberStatusChip,
  NoCategoriesWarning,
} from '@/components/members'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { formatDate } from '@/lib/format'
import { useUrlSort, useUrlState } from '@/lib/urlState'
import { cn } from '@/lib/utils'
import { createMemberSchema } from '@/lib/validation'

/*
 * Members (ADM-25): search, status and sort in the URL with a way to clear them, both request
 * modes labelled, created date; the list is filtered in the browser (a household's members fit).
 */

const STATUSES = ['active', 'suspended', 'retired'] as const satisfies MemberStatus[]

function matches(member: Member, query: string) {
  const q = query.toLowerCase()
  return [member.displayName, member.username, member.email ?? ''].some((v) =>
    v.toLowerCase().includes(q),
  )
}

export default function MembersPage() {
  const { t } = useTranslation(['admin', 'common'])
  const [query, setQuery] = useUrlState('q', z.string(), '')
  const [status, setStatus] = useUrlState<string>('status', z.enum(STATUSES), '')
  const [sort, setSort] = useUrlSort({ column: 'member', direction: 'asc' })
  const [, setParams] = useSearchParams()
  const [adding, setAdding] = useState(false)
  const members = useMembers({})
  const errorMessage = useApiErrorMessage()
  const filtered = Boolean(status || query.trim())
  const rows = members.data?.filter((m) => !status || m.status === status)

  const addButton = (
    <Button variant="primary" onClick={() => setAdding(true)}>
      <UserPlusIcon aria-hidden />
      {t('members.add')}
    </Button>
  )

  const columns: Column<Member>[] = [
    {
      id: 'member',
      header: t('members.columns.member'),
      className: 'min-w-64',
      sortValue: (m) => m.displayName,
      cell: (m) => (
        <Link
          to={`/admin/people/members/${m.id}`}
          className="flex flex-col rounded-control leading-tight"
        >
          <span className="font-medium text-text hover:underline">{m.displayName}</span>
          <span className="type-data text-sm text-text-3">
            {m.username}
            {m.email ? ` · ${m.email}` : ''}
          </span>
        </Link>
      ),
    },
    {
      id: 'status',
      header: t('members.columns.status'),
      className: 'w-40',
      sortValue: (m) => m.status,
      cell: (m) => <MemberStatusChip member={m} />,
    },
    {
      id: 'categories',
      header: t('members.columns.categories'),
      className: 'min-w-56',
      cell: (m) => (
        <CategoryList entitlements={m.entitlements} empty={t('members.noCategories')} modes />
      ),
    },
    {
      id: 'created',
      header: t('members.columns.created'),
      align: 'end',
      className: 'w-36',
      sortValue: (m) => m.createdAt,
      cell: (m) => <span className="type-data text-sm text-text-3">{formatDate(m.createdAt)}</span>,
    },
  ]

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('members.title')}
        description={t('members.description')}
        actions={members.data?.length ? addButton : undefined}
      />

      {members.data && members.data.length === 0 ? (
        <EmptyState
          icon={UsersIcon}
          title={t('members.emptyTitle')}
          description={t('members.emptyDescription')}
          action={addButton}
        />
      ) : (
        <DataTable
          label={t('members.title')}
          columns={columns}
          rows={rows}
          rowKey={(m) => m.id}
          sort={sort}
          onSortChange={setSort}
          search={{
            label: t('members.searchPlaceholder'),
            value: query,
            onChange: setQuery,
            matches,
          }}
          filters={
            <>
              <div
                role="radiogroup"
                aria-label={t('members.filterLabel')}
                className="inline-flex flex-wrap rounded-full border border-line bg-night-1 p-0.5"
              >
                {(['', ...STATUSES] as const).map((value) => (
                  <button
                    key={value || 'all'}
                    type="button"
                    role="radio"
                    aria-checked={status === value}
                    onClick={() => setStatus(value)}
                    className={cn(
                      'cursor-pointer rounded-full px-3 py-1 text-sm text-text-2 transition-colors hover:text-text max-lg:min-h-11',
                      status === value && 'bg-night-2 text-text',
                    )}
                  >
                    {value ? t(`common:memberStatus.${value}`) : t('members.filterAll')}
                  </button>
                ))}
              </div>
              {filtered && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() =>
                    // One update: two setters in one tick would each start from the same URL.
                    setParams(
                      (p) => {
                        p.delete('q')
                        p.delete('status')
                        return p
                      },
                      { replace: true },
                    )
                  }
                >
                  {t('members.clearFilters')}
                </Button>
              )}
            </>
          }
          loading={members.isPending}
          error={members.isError ? errorMessage(members.error) : null}
          onRetry={() => void members.refetch()}
          empty={
            <>
              <span className="block font-medium text-text">{t('members.noMatchesTitle')}</span>
              {t('members.noMatchesDescription')}
            </>
          }
        />
      )}

      <AddMemberDialog open={adding} onOpenChange={setAdding} />
    </>
  )
}

type AddValues = z.infer<typeof createMemberSchema>

function AddMemberDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useTranslation(['admin', 'auth', 'common'])
  const create = useCreateMember()
  const errorMessage = useApiErrorMessage()
  const [entitlements, setEntitlements] = useState<Entitlement[]>(defaultEntitlements)
  const [created, setCreated] = useState<{ name: string; link: OneTimeLink } | null>(null)
  const {
    register,
    handleSubmit,
    reset,
    formState: { errors, isSubmitting },
  } = useForm<AddValues>({
    resolver: zodResolver(createMemberSchema),
    defaultValues: { username: '', email: '', displayName: '' },
  })

  const close = (next: boolean) => {
    onOpenChange(next)
    if (!next) {
      reset()
      create.reset()
      setCreated(null)
      setEntitlements(defaultEntitlements())
    }
  }

  const onSubmit = handleSubmit(async (values) => {
    const result = await create
      .mutateAsync({ ...values, displayName: values.displayName || undefined, entitlements })
      .catch(() => null)
    if (result) setCreated({ name: result.member.displayName, link: result.setPasswordLink })
  })

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent
        className="max-w-lg"
        // The link is shown once: a stray click outside does not close it (Esc and Done do).
        onInteractOutside={(e) => {
          if (created) e.preventDefault()
        }}
      >
        {created ? (
          <>
            <DialogHeader>
              <DialogTitle>{t('members.addDialog.createdTitle')}</DialogTitle>
              <DialogDescription>
                {t('members.addDialog.createdDescription', {
                  name: created.name,
                  date: formatDate(created.link.expiresAt),
                })}
              </DialogDescription>
            </DialogHeader>
            <DialogBody>
              <CopyLink url={created.link.url} label={t('memberDetail.resetTitle')} />
            </DialogBody>
            <DialogFooter>
              <Button onClick={() => close(false)}>{t('members.addDialog.done')}</Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={onSubmit} noValidate className="flex flex-col gap-5">
            <DialogHeader>
              <DialogTitle>{t('members.addDialog.title')}</DialogTitle>
              <DialogDescription>{t('members.addDialog.description')}</DialogDescription>
            </DialogHeader>
            <DialogBody>
              {create.isError && <ErrorNotice>{errorMessage(create.error)}</ErrorNotice>}
              <FormField
                label={t('auth:fields.username')}
                hint={t('members.addDialog.usernameHint')}
                error={errors.username?.message}
                autoCapitalize="none"
                spellCheck={false}
                autoComplete="off"
                {...register('username')}
              />
              <FormField
                label={t('auth:fields.email')}
                error={errors.email?.message}
                type="email"
                autoComplete="off"
                {...register('email')}
              />
              <FormField
                label={t('auth:fields.displayName')}
                hint={t('members.addDialog.displayNameHint')}
                error={errors.displayName?.message}
                autoComplete="off"
                {...register('displayName')}
              />
              <fieldset className="flex flex-col gap-2">
                <legend className="mb-1.5 text-sm font-medium text-text-2">
                  {t('members.addDialog.entitlements')}
                </legend>
                <EntitlementsEditor
                  value={entitlements}
                  onChange={setEntitlements}
                  adultHint={t('memberDetail.adultHint')}
                />
              </fieldset>
              <NoCategoriesWarning entitlements={entitlements} />
            </DialogBody>
            <DialogFooter>
              <Button type="button" variant="ghost" onClick={() => close(false)}>
                {t('common:actions.cancel')}
              </Button>
              <Button type="submit" variant="primary" disabled={isSubmitting}>
                {t('members.addDialog.submit')}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}
