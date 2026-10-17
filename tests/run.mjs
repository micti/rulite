// Global setup / teardown for the integration tests:
//   1. rebuild temp copies of sample/novel.db, event.db and todo.db from their .sql files
//   2. add the users alice, bob and carol to todo.db with `rulite user add` (piped answers)
//      and read the API keys from `rulite keys`
//   3. start one target/release/rulite per database on a free port; todo.db with
//      `--config sample/todo.json` (auth on), the others without auth
//   4. run every *.test.mjs with `node --test`, one file at a time (they share the databases)
//   5. stop the servers and remove the temp copies
//
// Usage: npm test [-- extra node --test args]     RULITE_BIN overrides the binary path.

import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { DatabaseSync } from 'node:sqlite'

const here = dirname(fileURLToPath(import.meta.url))
const root = resolve(here, '..')
const bin = process.env.RULITE_BIN ?? join(root, 'target/release/rulite')

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

// The servers get no auth settings from the caller's environment.
const serverEnv = { ...process.env }
delete serverEnv.RULITE_CONFIG
delete serverEnv.RULITE_JWT_SECRET

function rulite(args, input) {
  return execFileSync(bin, args, { input, encoding: 'utf8', env: serverEnv })
}

// Binds port 0 and reads the real address from rulite's "serving ... on http://..." line.
function start(name, dbPath, extra = []) {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, [dbPath, '--bind', '127.0.0.1:0', ...extra], { stdio: ['ignore', 'inherit', 'pipe'], env: serverEnv })
    servers.push(child)
    let log = ''
    child.stderr.setEncoding('utf8')
    child.stderr.on('data', (chunk) => {
      log += chunk
      const m = log.match(/ on (http:\/\/\S+)\n/)
      if (m) resolve(m[1])
    })
    child.on('exit', (code) => reject(new Error(`rulite (${name}) exited with ${code}:\n${log}`)))
    child.on('error', reject)
  })
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
  const config = join(root, 'sample/todo.json')
  const todoDb = buildDb('todo')
  for (const user of ['alice', 'bob', 'carol']) {
    rulite(['user', 'add', todoDb, '--config', config], `${user}@example.com\nsecret1\nsecret1\n`)
  }
  const keys = Object.fromEntries(rulite(['keys', '--config', config]).trim().split('\n').map((l) => l.split(/\s+/)))
  const [novelUrl, eventUrl, todoUrl] = await Promise.all([
    start('novel', buildDb('novel')),
    start('event', buildDb('event')),
    start('todo', todoDb, ['--config', config]),
  ])
  const files = readdirSync(here).filter((f) => f.endsWith('.test.mjs')).sort()
  const args = ['--test', '--test-concurrency=1', ...process.argv.slice(2), ...files]
  const env = {
    ...process.env,
    RULITE_URL: novelUrl,
    RULITE_NOVEL_URL: novelUrl,
    RULITE_EVENT_URL: eventUrl,
    RULITE_TODO_URL: todoUrl,
    RULITE_TODO_CONFIG: config,
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
