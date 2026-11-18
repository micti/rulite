'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { nav } from '@/lib/nav'
import { cn } from '@/lib/utils'

function normalize(path: string) {
  return path.endsWith('/') ? path : `${path}/`
}

export function SidebarNav({ className }: { className?: string }) {
  const pathname = normalize(usePathname())
  return (
    <nav className={cn('flex flex-col gap-6', className)}>
      {nav.map((group) => (
        <div key={group.title} className="flex flex-col gap-1">
          <h4 className="text-muted-foreground px-2 pb-1 text-xs font-medium uppercase tracking-wide">{group.title}</h4>
          {group.items.map((item) => {
            const active = pathname === normalize(item.href)
            return (
              <Link
                key={item.href}
                href={item.href}
                aria-current={active ? 'page' : undefined}
                className={cn(
                  'rounded-md px-2 py-1.5 text-sm transition-colors',
                  active
                    ? 'bg-accent text-accent-foreground font-medium'
                    : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground',
                )}
              >
                {item.title}
              </Link>
            )
          })}
        </div>
      ))}
    </nav>
  )
}

export function MobileNav() {
  const pathname = normalize(usePathname())
  const items = nav.flatMap((g) => g.items)
  return (
    <nav className="flex gap-1 overflow-x-auto px-4 pb-2 md:hidden">
      {items.map((item) => {
        const active = pathname === normalize(item.href)
        return (
          <Link
            key={item.href}
            href={item.href}
            className={cn(
              'rounded-md px-3 py-1.5 text-sm whitespace-nowrap',
              active ? 'bg-accent text-accent-foreground font-medium' : 'text-muted-foreground',
            )}
          >
            {item.title}
          </Link>
        )
      })}
    </nav>
  )
}
