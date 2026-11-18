import type { Metadata } from 'next'
import { SidebarNav } from '@/components/sidebar-nav'
import { SiteHeader } from '@/components/site-header'
import { ThemeProvider } from '@/components/theme-provider'
import './globals.css'

export const metadata: Metadata = {
  title: { default: 'Rulite', template: '%s · Rulite' },
  description: 'A Supabase-compatible REST API for any SQLite database, written in Rust.',
}

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <body className="min-h-screen">
        <ThemeProvider attribute="class" defaultTheme="system" enableSystem disableTransitionOnChange>
          <SiteHeader />
          <div className="mx-auto grid max-w-6xl grid-cols-1 gap-10 px-4 md:grid-cols-[220px_minmax(0,1fr)]">
            <aside className="hidden md:block">
              <div className="sticky top-14 h-[calc(100vh-3.5rem)] overflow-y-auto py-8">
                <SidebarNav />
              </div>
            </aside>
            <main className="min-w-0 py-8 md:py-10">
              <article className="max-w-3xl">{children}</article>
            </main>
          </div>
        </ThemeProvider>
      </body>
    </html>
  )
}
