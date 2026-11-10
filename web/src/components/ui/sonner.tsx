import { Toaster as Sonner, type ToasterProps } from 'sonner'

export function Toaster(props: ToasterProps) {
  return (
    <Sonner
      theme="dark"
      position="bottom-right"
      toastOptions={{
        classNames: {
          toast:
            'rounded-card! border! border-line! bg-night-2! text-text! font-sans! text-base! shadow-none!',
          description: 'text-text-2!',
        },
      }}
      {...props}
    />
  )
}
