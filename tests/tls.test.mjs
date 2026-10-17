// M18: built-in HTTPS. Needs a binary built with the `tls` feature:
//   cargo build --release --features tls --target-dir target/tls
// (RULITE_TLS_BIN overrides the path). The test certificate in tls/ is trusted through
// NODE_EXTRA_CA_CERTS, set by `npm test`.

import { after, before, describe, test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { createClient } from '@supabase/supabase-js'

import { anonKey, ok } from './client.mjs'
import { root, serverEnv, start } from './server.mjs'

const tlsBin = process.env.RULITE_TLS_BIN ?? join(root, 'target/tls/release/rulite')
const skip = existsSync(tlsBin) ? false : `no TLS build at ${tlsBin}; run: cargo build --release --features tls --target-dir target/tls`

describe('tls', { skip }, () => {
  const dir = mkdtempSync(join(tmpdir(), 'rulite-tls-'))
  let server

  before(async () => {
    const dbPath = join(dir, 'novel.db')
    const db = new DatabaseSync(dbPath)
    db.exec(readFileSync(join(root, 'sample', 'novel.sql'), 'utf8'))
    db.close()
    const config = join(root, 'sample', 'novel.json')
    execFileSync(tlsBin, ['user', 'add', dbPath, '--config', config], { input: 'dana@example.com\nsecret1\nsecret1\n', env: serverEnv() })
    const pem = (name) => join(import.meta.dirname, 'tls', name)
    server = await start([dbPath, '--config', config, '--tls-cert', pem('cert.pem'), '--tls-key', pem('key.pem')], { program: tlsBin })
  })

  after(() => {
    server?.child.kill('SIGTERM')
    rmSync(dir, { recursive: true, force: true })
  })

  test('serves https', () => {
    assert.match(server.url, /^https:\/\/127\.0\.0\.1:\d+$/)
  })

  test('supabase-js reads and signs in over https', async () => {
    const client = createClient(server.url, anonKey, { auth: { persistSession: false, autoRefreshToken: false } })
    const rows = ok(await client.from('novel').select('title').eq('status', 'completed'))
    assert.deepEqual(rows, [{ title: 'The Lantern Road' }])
    const { session } = ok(await client.auth.signInWithPassword({ email: 'dana@example.com', password: 'secret1' }))
    const claims = JSON.parse(Buffer.from(session.access_token.split('.')[1], 'base64url'))
    assert.equal(claims.iss, `${server.url}/auth/v1`)
    assert.equal(ok(await client.auth.getUser()).user.email, 'dana@example.com')
  })

  test('plain HTTP to the TLS port fails', async () => {
    await assert.rejects(fetch(`${server.url.replace('https:', 'http:')}/rest/v1/novel`, { headers: { apikey: anonKey } }))
  })
})
