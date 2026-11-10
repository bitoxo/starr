import { useId, type ComponentProps } from 'react'
import { useTranslation } from 'react-i18next'

import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import type { ValidationKey } from '@/lib/validation'

/**
 * " (required)" after a label (spec 0008 F8, ADM-36): required inputs say so up front and are
 * checked on submit with the error at the field, instead of a silently disabled button.
 */
export function RequiredMark() {
  const { t } = useTranslation()
  return <span className="font-normal text-text-3"> {t('form.required')}</span>
}

/**
 * Labelled input with hint and validation message. `error` is a validation key
 * (zod messages are keys, see lib/validation.ts) and is translated here. `required` marks the
 * label.
 */
export function FormField({
  label,
  hint,
  error,
  required,
  ...inputProps
}: ComponentProps<typeof Input> & { label: string; hint?: string; error?: string }) {
  const { t } = useTranslation('auth')
  const id = useId()
  const hintId = `${id}-hint`
  const errorId = `${id}-error`
  const describedBy = [hint && hintId, error && errorId].filter(Boolean).join(' ') || undefined

  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>
        {label}
        {required && <RequiredMark />}
      </Label>
      <Input
        id={id}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy}
        {...inputProps}
      />
      {hint && !error && (
        <p id={hintId} className="text-sm text-text-3">
          {hint}
        </p>
      )}
      {error && (
        <p id={errorId} className="text-sm text-bad">
          {t(`validation.${error as ValidationKey}`)}
        </p>
      )}
    </div>
  )
}
