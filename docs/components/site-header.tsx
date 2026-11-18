import Link from 'next/link'
import { Database } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { MobileNav } from '@/components/sidebar-nav'
import { ThemeToggle } from '@/components/theme-toggle'
import { repo } from '@/lib/nav'

export function SiteHeader() {
  return (
    <header className="bg-background/90 sticky top-0 z-40 border-b backdrop-blur">
      <div className="mx-auto flex h-14 max-w-6xl items-center gap-3 px-4">
        <Link href="/" className="flex items-center gap-2 font-semibold">
          <Database className="size-5" />
          Rulite
        </Link>
        <Badge variant="secondary">v0.2.0</Badge>
        <div className="ml-auto flex items-center gap-1">
          <Button variant="ghost" size="sm" asChild>
            <a href={repo}>GitHub</a>
          </Button>
          <ThemeToggle />
        </div>
      </div>
      <MobileNav />
    </header>
  )
}
