import type { Metadata } from 'next'
import { TriangleAlert } from 'lucide-react'
import { CodeBlock } from '@/components/code-block'
import { Pager } from '@/components/pager'
import { A, Code, H2, H3, P, PageHeader, Ul } from '@/components/prose'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

export const metadata: Metadata = { title: 'Usage' }

const config = [
  ['jwt_secret', 'required', 'HS256 secret, at least 32 characters. Overridden by RULITE_JWT_SECRET.'],
  ['jwt_exp', '3600', 'Access token lifetime in seconds.'],
  ['external_url', 'http://<bind>', 'Public base URL; the token iss is <external_url>/auth/v1.'],
  ['refresh_token_reuse_interval', '10', 'Seconds an already-used refresh token is still accepted.'],
  ['password_min_length', '6', 'For user add, user passwd and updateUser({ password }).'],
  ['tables', '{}', 'Permissions per table. Without a grant only service_role works.'],
  ['functions', 'none', 'Folder of *.sql function files, relative to the config file.'],
  ['rpc', '{}', 'Who may call each function.'],
  ['cors_origins', 'none', 'Origins a browser app may call from, or "*".'],
  ['tls_cert, tls_key', 'none', 'PEM chain and key for built-in HTTPS (--features tls).'],
]

const options = [
  ['--bind <addr>', 'RULITE_BIND', '127.0.0.1:3000'],
  ['--readers <n>', 'RULITE_READERS', '1 (1–2)'],
  ['--default-limit <n>', 'RULITE_DEFAULT_LIMIT', '100'],
  ['--max-limit <n>', 'RULITE_MAX_LIMIT', '1000'],
  ['--config <file>', 'RULITE_CONFIG', ''],
  ['--cors-origins <list>', 'RULITE_CORS_ORIGINS', ''],
  ['--tls-cert <pem>', 'RULITE_TLS_CERT', ''],
  ['--tls-key <pem>', 'RULITE_TLS_KEY', ''],
]

export default function Page() {
  return (
    <>
      <PageHeader
        title="Usage"
        lead="Write a config, create keys and users, start the server, then use supabase-js as you would with Supabase."
      />

      <H2 id="quick-start">Quick start</H2>
      <P>A config file and three commands take a SQLite file to a working API:</P>
      <CodeBlock
        title="rulite.json"
        lang="json"
        code={`
{
  "jwt_secret": "change-me-to-a-long-random-string-0123456789",
  "tables": {
    "todo": { "authenticated": "write", "owner": "user_id" }
  }
}
`}
      />
      <CodeBlock
        code={`
rulite keys --config rulite.json        # 1. anon (public) and service_role (private) keys
rulite user add app.db                  # 2. prompts for email and password
rulite app.db --config rulite.json      # 3. serve on http://127.0.0.1:3000
`}
      />
      <CodeBlock
        lang="ts"
        code={`
import { createClient } from '@supabase/supabase-js'

const supabase = createClient('http://127.0.0.1:3000', ANON_KEY)
await supabase.auth.signInWithPassword({ email: 'alice@example.com', password: 'secret1' })
const { data } = await supabase.from('todo').select('*')   // only alice's todos
`}
      />

      <H2 id="config">Config file</H2>
      <P>
        The config is a JSON file passed with <Code>--config</Code> (or <Code>RULITE_CONFIG</Code>). Paths in it are
        relative to the file. Command-line options win over environment variables, which win over the file.
      </P>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Key</TableHead>
            <TableHead>Default</TableHead>
            <TableHead>Meaning</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {config.map(([key, def, text]) => (
            <TableRow key={key}>
              <TableCell>
                <Code>{key}</Code>
              </TableCell>
              <TableCell className="text-muted-foreground whitespace-nowrap">{def}</TableCell>
              <TableCell>{text}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>

      <H2 id="permissions">Roles and permissions</H2>
      <P>Every request runs as one of three roles, decided by the key or token the client sends:</P>
      <Ul>
        <li>
          <Code>anon</Code>: the anon key, safe in a browser. Sees what tables grant <Code>anon</Code>.
        </li>
        <li>
          <Code>authenticated</Code>: a signed-in user&apos;s access token. Sees what tables grant{' '}
          <Code>authenticated</Code>, and only their own rows on owner tables.
        </li>
        <li>
          <Code>service_role</Code>: the private service key, for seed scripts and admin jobs. Every table, every row.
        </li>
      </Ul>
      <P>
        Each table grants <Code>none</Code>, <Code>read</Code> or <Code>write</Code> per role; <Code>&quot;*&quot;</Code>{' '}
        sets the default. An <Code>owner</Code> column limits signed-in users to their rows, and{' '}
        <Code>owner_scope: &quot;write&quot;</Code> lets them read every row but write only their own.
      </P>
      <CodeBlock
        lang="json"
        code={`
{
  "tables": {
    "tag":     { "anon": "read", "authenticated": "read" },
    "todo":    { "authenticated": "write", "owner": "user_id" },
    "comment": { "anon": "read", "authenticated": "write", "owner": "author_id", "owner_scope": "write" }
  }
}
`}
      />
      <P>
        An owner column holds the user&apos;s UUID, so app tables can reference the users table:{' '}
        <Code>user_id TEXT REFERENCES auth_users(id) ON DELETE CASCADE</Code>. On insert a missing owner is filled in
        with the user&apos;s id.
      </P>

      <H2 id="users">Users</H2>
      <P>There is no sign-up endpoint; the operator manages users from the command line, also while the server runs:</P>
      <CodeBlock
        code={`
rulite user add <db>              # asks email and password; the account is active at once
rulite user passwd <db> <email>   # new password; signs the user out everywhere
rulite user delete <db> <email>   # removes the user and their sessions
rulite user list <db>

# in scripts, pipe the answers
printf 'alice@example.com\\nsecret1\\nsecret1\\n' | rulite user add app.db
`}
      />
      <P>
        supabase-js supports <Code>signInWithPassword</Code>, <Code>getUser</Code>, <Code>getSession</Code>, refresh,{' '}
        <Code>updateUser</Code> and <Code>signOut</Code>.
      </P>

      <H2 id="queries">Querying</H2>
      <P>Tables and views are served at <Code>/rest/v1/&lt;name&gt;</Code>. Everything supabase-js builds works:</P>
      <CodeBlock
        lang="ts"
        code={`
// nested select through foreign keys
const { data } = await supabase
  .from('novel')
  .select('title, edition(language, volume(number, chapter(title)))')
  .eq('title', 'The Lantern Road')
  .single()

// filters, order, range and an exact count
const { data: rows, count } = await supabase
  .from('novel')
  .select('*', { count: 'exact' })
  .ilike('title', '%road%')
  .order('title')
  .range(0, 9)

// writes
await supabase.from('todo').insert({ title: 'Buy milk' })
await supabase.from('todo').update({ done: true }).eq('id', id)
await supabase.from('todo').upsert(rows, { onConflict: 'id' })
await supabase.from('todo').delete().eq('done', true)
`}
      />
      <P>
        Requests without a limit return 100 rows, and no request returns more than 1000 (<Code>--default-limit</Code>,{' '}
        <Code>--max-limit</Code>). Errors use PostgREST&apos;s <Code>{'{code, message, details, hint}'}</Code> shape.
      </P>

      <H2 id="functions">Functions (RPC)</H2>
      <P>
        SQLite has no stored functions, so a function is a <Code>.sql</Code> file in the folder the config names. The
        file name is the function name; the header declares its arguments and result:
      </P>
      <CodeBlock
        title="functions/search_novels.sql"
        lang="sql"
        code={`
-- Novels whose title contains q.
-- @param q text
-- @param max integer = 50
-- @returns setof
SELECT id, title, status FROM novel WHERE title LIKE '%' || :q || '%' LIMIT :max;
`}
      />
      <CodeBlock
        lang="ts"
        code={`
const { data } = await supabase.rpc('search_novels', { q: 'Road' }).eq('status', 'completed')
`}
      />
      <Ul>
        <li>
          Argument types: <Code>text</Code>, <Code>integer</Code>, <Code>real</Code>, <Code>boolean</Code>,{' '}
          <Code>json</Code>; a default makes the argument optional.
        </li>
        <li>
          Results: <Code>setof</Code> (default), <Code>row</Code>, <Code>value</Code>, <Code>json</Code> or{' '}
          <Code>void</Code>.
        </li>
        <li>
          <Code>:auth_uid</Code> and <Code>:auth_role</Code> are always bound, so a function can limit rows to the
          caller.
        </li>
        <li>A read-only file runs on a reader; a file that writes runs in one transaction on the writer.</li>
        <li>
          Only <Code>service_role</Code> may call a function unless the config&apos;s <Code>rpc</Code> grants{' '}
          <Code>execute</Code>.
        </li>
      </Ul>
      <CodeBlock
        lang="json"
        code={`
{
  "functions": "functions",
  "rpc": {
    "*":      { "authenticated": "execute" },
    "whoami": { "anon": "execute" }
  }
}
`}
      />

      <H2 id="types">TypeScript types</H2>
      <P>
        Rulite prints the same <Code>Database</Code> type as <Code>supabase gen types typescript</Code>, from the file
        or from a running server (with the service key):
      </P>
      <CodeBlock
        code={`
rulite gen types app.db --config app.json > database.types.ts
curl -H "apikey: $SERVICE_KEY" http://127.0.0.1:3000/pg/generators/typescript > database.types.ts
`}
      />
      <CodeBlock
        lang="ts"
        code={`
import type { Database } from './database.types'

const supabase = createClient<Database>('http://127.0.0.1:3000', ANON_KEY)
`}
      />

      <H2 id="deploy">Deploy</H2>
      <H3 id="proxy">Behind a reverse proxy (recommended)</H3>
      <P>
        Keep Rulite on localhost and let a proxy such as <A href="https://caddyserver.com">Caddy</A> handle HTTPS and
        certificates. Set <Code>external_url</Code> to the public URL.
      </P>
      <CodeBlock
        lang="text"
        title="Caddyfile"
        code={`
api.example.com {
	reverse_proxy 127.0.0.1:3000
}
`}
      />
      <H3 id="cors">CORS</H3>
      <P>A browser app on another origin needs its origin allowed:</P>
      <CodeBlock code="rulite app.db --config app.json --cors-origins http://localhost:5173,https://app.example.com" />
      <H3 id="tls">Built-in TLS</H3>
      <P>
        For a LAN or a quick deploy without a proxy, a build with <Code>--features tls</Code> serves HTTPS itself (TLS
        1.2 / 1.3 over HTTP/1.1, no automatic certificates):
      </P>
      <CodeBlock code="rulite app.db --config app.json --bind 0.0.0.0:443 --tls-cert cert.pem --tls-key key.pem" />
      <Alert className="my-4">
        <TriangleAlert />
        <AlertTitle>Keep the service key private</AlertTitle>
        <AlertDescription>
          The service_role key bypasses every permission. Never ship it to a browser, and use a long random{' '}
          <Code>jwt_secret</Code> in production.
        </AlertDescription>
      </Alert>

      <H2 id="cli">CLI reference</H2>
      <CodeBlock
        lang="text"
        code={`
rulite [serve] <db-path> [options]
rulite user add <db-path>              [--config <file>]
rulite user passwd <db-path> <email>   [--config <file>]
rulite user delete <db-path> <email>
rulite user list <db-path>
rulite keys                            [--config <file>]
rulite gen types <db-path>             [--config <file>]
`}
      />
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Option</TableHead>
            <TableHead>Env var</TableHead>
            <TableHead>Default</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {options.map(([opt, env, def]) => (
            <TableRow key={opt}>
              <TableCell>
                <Code>{opt}</Code>
              </TableCell>
              <TableCell>
                <Code>{env}</Code>
              </TableCell>
              <TableCell className="text-muted-foreground">{def || '—'}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <P>
        The database path may also come from <Code>RULITE_DB</Code>, and <Code>RULITE_JWT_SECRET</Code> overrides the
        config&apos;s secret.
      </P>

      <Pager href="/usage/" />
    </>
  )
}
