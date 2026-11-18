import { codeToHtml } from 'shiki'

/** Syntax-highlighted code, rendered at build time with light and dark themes. */
export async function CodeBlock({ code, lang = 'bash', title }: { code: string; lang?: string; title?: string }) {
  const html = await codeToHtml(code.trim(), {
    lang,
    themes: { light: 'github-light', dark: 'github-dark' },
    defaultColor: 'light',
  })
  return (
    <div className="bg-code my-4 overflow-hidden rounded-lg border">
      {title && <div className="text-muted-foreground border-b px-4 py-2 font-mono text-xs">{title}</div>}
      <div
        className="overflow-x-auto px-4 py-3 text-[13px] leading-relaxed [&_pre]:font-mono"
        dangerouslySetInnerHTML={{ __html: html }}
      />
    </div>
  )
}
