import type { Metadata } from 'next'
import { Info } from 'lucide-react'
import { CodeBlock } from '@/components/code-block'
import { Pager } from '@/components/pager'
import { A, Code, H2, P, PageHeader, Ul } from '@/components/prose'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'

export const metadata: Metadata = { title: 'Install' }

export default function Page() {
  return (
    <>
      <PageHeader title="Install" lead="Rulite is a single binary with SQLite bundled, so nothing else is needed at runtime. Download a release or build it with Cargo." />

      <H2 id="binary">Download a binary</H2>
      <P>
        Each <A href="https://github.com/micti/rulite/releases">release</A> has prebuilt binaries for Linux (x86-64,
        arm64) and macOS (Apple silicon), each also as a <Code>-tls</Code> variant with built-in HTTPS:
      </P>
      <Tabs defaultValue="linux-x64" className="my-4">
        <TabsList>
          <TabsTrigger value="linux-x64">Linux x86-64</TabsTrigger>
          <TabsTrigger value="linux-arm64">Linux arm64</TabsTrigger>
          <TabsTrigger value="macos">macOS</TabsTrigger>
        </TabsList>
        {[
          ['linux-x64', 'x86_64-unknown-linux-gnu'],
          ['linux-arm64', 'aarch64-unknown-linux-gnu'],
          ['macos', 'aarch64-apple-darwin'],
        ].map(([value, target]) => (
          <TabsContent key={value} value={value}>
            <CodeBlock
              code={`
curl -fsSL https://github.com/micti/rulite/releases/latest/download/rulite-${target}.tar.gz | tar -xz
sudo mv rulite-${target}/rulite /usr/local/bin/
`}
            />
          </TabsContent>
        ))}
      </Tabs>
      <P>
        Each archive has a <Code>.sha256</Code> file next to it for checking the download. For other platforms, install
        with Cargo.
      </P>

      <H2 id="requirements">Requirements to build</H2>
      <Ul>
        <li>
          Rust <strong>1.91</strong> or newer (edition 2024), from <A href="https://rustup.rs">rustup.rs</A>.
        </li>
        <li>A C compiler (for the bundled SQLite): Xcode command-line tools, <Code>build-essential</Code> or MSVC.</li>
        <li>
          Optional: Node.js 22.13+ to run the supabase-js integration tests, <Code>sqlite3</Code> to build the sample
          databases.
        </li>
      </Ul>

      <H2 id="cargo">Install with Cargo</H2>
      <Tabs defaultValue="default" className="my-4">
        <TabsList>
          <TabsTrigger value="default">Default</TabsTrigger>
          <TabsTrigger value="tls">With built-in TLS</TabsTrigger>
        </TabsList>
        <TabsContent value="default">
          <CodeBlock code="cargo install --git https://github.com/micti/rulite" />
        </TabsContent>
        <TabsContent value="tls">
          <CodeBlock code="cargo install --git https://github.com/micti/rulite --features tls" />
        </TabsContent>
      </Tabs>
      <P>
        This puts <Code>rulite</Code> in <Code>~/.cargo/bin</Code>. The <Code>tls</Code> feature adds rustls (about
        0.8 MB) so Rulite can serve HTTPS itself; the default build has no TLS crates.
      </P>

      <H2 id="source">Build from source</H2>
      <CodeBlock
        code={`
git clone https://github.com/micti/rulite
cd rulite
cargo build --release                      # target/release/rulite
cargo build --release --features tls       # optional: built-in HTTPS
`}
      />
      <P>
        The release profile uses fat LTO, one codegen unit, <Code>panic = &quot;abort&quot;</Code> and strips symbols,
        so the binary is about 3 MB.
      </P>

      <H2 id="verify">Verify</H2>
      <CodeBlock
        code={`
rulite --help
`}
      />
      <CodeBlock
        lang="text"
        title="output"
        code={`
Usage: rulite [serve] <db-path> [options]
       rulite user add <db-path>              [--config <file>]
       rulite user passwd <db-path> <email>   [--config <file>]
       rulite user delete <db-path> <email>
       rulite user list <db-path>
       rulite keys                            [--config <file>]
       rulite gen types <db-path>             [--config <file>]  TypeScript types for supabase-js
`}
      />

      <H2 id="sample">Try the samples</H2>
      <P>The repository ships sample schemas and configs. Build a database and serve it:</P>
      <CodeBlock
        code={`
sqlite3 sample/novel.db < sample/novel.sql
rulite keys --config sample/novel.json          # prints the anon and service_role keys
rulite sample/novel.db --config sample/novel.json
`}
      />
      <Alert className="my-4">
        <Info />
        <AlertTitle>Development secrets</AlertTitle>
        <AlertDescription>
          The sample configs share a public <Code>jwt_secret</Code> and grant <Code>anon</Code> write access. Use them
          only locally.
        </AlertDescription>
      </Alert>

      <Pager href="/install/" />
    </>
  )
}
