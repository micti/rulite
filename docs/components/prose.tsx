import { cn } from '@/lib/utils'

export function PageHeader({ title, lead }: { title: string; lead: React.ReactNode }) {
  return (
    <div className="mb-8 flex flex-col gap-3">
      <h1 className="text-3xl font-bold tracking-tight md:text-4xl">{title}</h1>
      <p className="text-muted-foreground text-lg">{lead}</p>
    </div>
  )
}

export function H2({ id, children }: { id: string; children: React.ReactNode }) {
  return (
    <h2 id={id} className="group mt-12 mb-4 scroll-m-20 border-b pb-2 text-2xl font-semibold tracking-tight first:mt-0">
      <a href={`#${id}`}>{children}</a>
    </h2>
  )
}

export function H3({ id, children }: { id: string; children: React.ReactNode }) {
  return (
    <h3 id={id} className="mt-8 mb-3 scroll-m-20 text-lg font-semibold tracking-tight">
      <a href={`#${id}`}>{children}</a>
    </h3>
  )
}

export function P({ className, ...props }: React.ComponentProps<'p'>) {
  return <p className={cn('my-4 leading-7', className)} {...props} />
}

export function Ul({ className, ...props }: React.ComponentProps<'ul'>) {
  return <ul className={cn('my-4 ml-6 list-disc space-y-2 leading-7', className)} {...props} />
}

export function Code(props: React.ComponentProps<'code'>) {
  return <code className="bg-muted rounded px-1.5 py-0.5 font-mono text-[0.85em]" {...props} />
}

export function A(props: React.ComponentProps<'a'>) {
  return <a className="font-medium underline underline-offset-4" {...props} />
}
