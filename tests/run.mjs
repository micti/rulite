// Global setup / teardown for the integration tests:
//   1. rebuild temp copies of sample/novel.db, event.db and todo.db from their .sql files
//   2. add the users alice, bob and carol to todo.db with `rulite user add` (piped answers)
//      and read the API keys from `rulite keys`
//   3. start one target/release/rulite per database on a free port, each with its
//      sample/<name>.json config (one jwt_secret, so one key pair works on all three);
//      novel also allows the CORS origins in CORS_ORIGINS
//   4. run every *.test.mjs with `node --test`, one file at a time (they share the databases)
//   5. stop the servers and remove the temp copies
//
// Usage: npm test [-- extra node --test args]     RULITE_BIN overrides the binary path.

import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { DatabaseSync } from 'node:sqlite'

import { bin, root, serverEnv, start } from './server.mjs'

const here = dirname(fileURLToPath(import.meta.url))

if (!existsSync(bin)) {
  console.error(`rulite binary not found at ${bin}; run \`cargo build --release\` first`)
  process.exit(1)
}

const dir = mkdtempSync(join(tmpdir(), 'rulite-test-'))
const servers = []

function buildDb(name) {
  const path = join(dir, `${name}.db`)
  const db = new DatabaseSync(path)
  db.exec(readFileSync(join(root, 'sample', `${name}.sql`), 'utf8'))
  db.close()
  return path
}

function rulite(args, input) {
  return execFileSync(bin, args, { input, encoding: 'utf8', env: serverEnv() })
}

const config = (name) => join(root, 'sample', `${name}.json`)
const CORS_ORIGINS = 'http://localhost:5173,https://app.example.com'

async function serve(name, dbPath = buildDb(name), extra = []) {
  const { url, child } = await start([dbPath, '--config', config(name), ...extra])
  servers.push(child)
  return url
}

function stop() {
  for (const child of servers) child.kill('SIGTERM')
  rmSync(dir, { recursive: true, force: true })
}

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    stop()
    process.exit(130)
  })
}

let status = 1
try {
  const todoDb = buildDb('todo')
  for (const user of ['alice', 'bob', 'carol']) {
    rulite(['user', 'add', todoDb, '--config', config('todo')], `${user}@example.com\nsecret1\nsecret1\n`)
  }
  const keys = Object.fromEntries(rulite(['keys', '--config', config('todo')]).trim().split('\n').map((l) => l.split(/\s+/)))
  const [novelUrl, eventUrl, todoUrl] = await Promise.all([
    serve('novel', undefined, ['--cors-origins', CORS_ORIGINS]),
    serve('event'),
    serve('todo', todoDb),
  ])
  const files = readdirSync(here).filter((f) => f.endsWith('.test.mjs')).sort()
  const args = ['--test', '--test-concurrency=1', ...process.argv.slice(2), ...files]
  const env = {
    ...process.env,
    RULITE_BIN: bin,
    RULITE_URL: novelUrl,
    RULITE_NOVEL_URL: novelUrl,
    RULITE_EVENT_URL: eventUrl,
    RULITE_TODO_URL: todoUrl,
    RULITE_TODO_CONFIG: config('todo'),
    RULITE_CORS_ORIGINS: CORS_ORIGINS,
    RULITE_ANON_KEY: keys.anon,
    RULITE_SERVICE_KEY: keys.service_role,
  }
  status = await new Promise((resolve) => {
    spawn(process.execPath, args, { cwd: here, env, stdio: 'inherit' }).on('exit', (code) => resolve(code ?? 1))
  })
} catch (e) {
  console.error(e.message)
} finally {
  stop()
}
process.exit(status)
