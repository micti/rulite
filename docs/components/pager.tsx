import Link from 'next/link'
import { ArrowLeft, ArrowRight } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { nav } from '@/lib/nav'

export function Pager({ href }: { href: string }) {
  const items = nav.flatMap((g) => g.items)
  const i = items.findIndex((item) => item.href === href)
  const prev = items[i - 1]
  const next = items[i + 1]
  return (
    <>
      <Separator className="mt-16 mb-6" />
      <div className="flex justify-between gap-4">
        {prev ? (
          <Button variant="outline" asChild>
            <Link href={prev.href}>
              <ArrowLeft /> {prev.title}
            </Link>
          </Button>
        ) : (
          <span />
        )}
        {next && (
          <Button variant="outline" asChild>
            <Link href={next.href}>
              {next.title} <ArrowRight />
            </Link>
          </Button>
        )}
      </div>
    </>
  )
}
