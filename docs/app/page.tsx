import Link from 'next/link'
import { ArrowRight, Braces, FileCode2, Gauge, KeyRound, Lock, Table2 } from 'lucide-react'
import { CodeBlock } from '@/components/code-block'
import { Pager } from '@/components/pager'
import { A, Code, H2, P, PageHeader, Ul } from '@/components/prose'
import { Button } from '@/components/ui/button'
import { Card, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

const features = [
  { icon: Table2, title: 'REST from your schema', text: 'Tables, views and foreign-key embeds at /rest/v1, the way PostgREST serves them.' },
  { icon: KeyRound, title: 'Supabase-style auth', text: 'Email + password sign-in with GoTrue-compatible tokens and anon, authenticated, service_role.' },
  { icon: Lock, title: 'Table permissions', text: 'none, read or write per role and table, plus owner rows that limit users to their own data.' },
  { icon: FileCode2, title: 'Functions as SQL files', text: 'A .sql file with a small header becomes a function for supabase.rpc().' },
  { icon: Braces, title: 'TypeScript types', text: 'The same Database type as supabase gen types, so createClient<Database>() type-checks.' },
  { icon: Gauge, title: 'Tiny footprint', text: 'A 3 MB binary, ~4 MB idle memory and ~13k req/s on a 5-level nested query.' },
]

export default function Page() {
  return (
    <>
      <PageHeader
        title="Introduction"
        lead="Rulite is a Supabase-compatible REST API for any SQLite database, written in Rust. Point it at a .db file and talk to it with the official supabase-js client."
      />
      <div className="flex flex-wrap gap-2">
        <Button asChild>
          <Link href="/install/">
            Install <ArrowRight />
          </Link>
        </Button>
        <Button variant="outline" asChild>
          <Link href="/usage/">Usage</Link>
        </Button>
      </div>

      <H2 id="what">What it is</H2>
      <P>
        Rulite reads the schema of a SQLite file and serves it as a PostgREST-style API under{' '}
        <Code>/rest/v1</Code>, with a GoTrue-style auth API under <Code>/auth/v1</Code>. Code written against Supabase
        works unchanged: filters, ordering, ranges, nested selects, inserts, upserts, updates, deletes, sign-in and{' '}
        <Code>rpc()</Code>.
      </P>
      <CodeBlock
        lang="ts"
        code={`
import { createClient } from '@supabase/supabase-js'

const supabase = createClient('http://127.0.0.1:3000', ANON_KEY)

const { data } = await supabase
  .from('novel')
  .select('title, edition(language, volume(number))')
  .eq('status', 'completed')
  .order('title')
`}
      />
      <P>
        It is a single binary with no external services: one SQLite file holds your tables and the users. That makes it
        a good fit for local development, tests, prototypes, small apps, and embedded or edge devices.
      </P>

      <H2 id="features">Features</H2>
      <div className="grid gap-4 sm:grid-cols-2">
        {features.map(({ icon: Icon, title, text }) => (
          <Card key={title} className="gap-3 py-5">
            <CardHeader className="px-5">
              <Icon className="text-muted-foreground mb-1 size-5" />
              <CardTitle>{title}</CardTitle>
              <CardDescription>{text}</CardDescription>
            </CardHeader>
          </Card>
        ))}
      </div>

      <H2 id="how">How it works</H2>
      <Ul>
        <li>
          <strong>JSON built by SQLite.</strong> A read is one SQL statement that returns the finished JSON body (
          <Code>json_group_array(json_object(...))</Code>), so nested embeds cost one query.
        </li>
        <li>
          <strong>One writer, one reader.</strong> A writer thread runs every write in one <Code>IMMEDIATE</Code>{' '}
          transaction; reader threads use read-only connections in WAL mode.
        </li>
        <li>
          <strong>Never request text in SQL.</strong> Names are resolved through the introspected schema and every value
          is a bound parameter.
        </li>
        <li>
          <strong>Auth always on.</strong> The server needs a <Code>jwt_secret</Code>, every request carries a key, and
          tables are closed to <Code>anon</Code> and <Code>authenticated</Code> until the config grants them.
        </li>
      </Ul>

      <H2 id="compat">Compared with Supabase</H2>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Feature</TableHead>
            <TableHead>Rulite</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow>
            <TableCell>Tables, views, embeds, filters</TableCell>
            <TableCell>Yes (views are read-only)</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>Email + password auth</TableCell>
            <TableCell>Yes; users are added with the CLI, no sign-up endpoint</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>Row Level Security</TableCell>
            <TableCell>Per-table grants and owner columns in a JSON config</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>Database functions</TableCell>
            <TableCell>
              <Code>.sql</Code> files with a header
            </TableCell>
          </TableRow>
          <TableRow>
            <TableCell>Realtime, storage, OAuth, magic links</TableCell>
            <TableCell>No</TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <P>
        For the full design see <A href="https://github.com/micti/rulite/blob/main/ARCHITECTURE.md">ARCHITECTURE.md</A>{' '}
        and <A href="https://github.com/micti/rulite/blob/main/AUTH.md">AUTH.md</A>.
      </P>

      <Pager href="/" />
    </>
  )
}
