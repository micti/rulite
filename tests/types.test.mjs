// M15 / M16: TypeScript types at /pg/generators/typescript (service_role only) and from
// `rulite gen types`; the generated files type-check supabase-js calls (types/usage.ts).

import { describe, test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'

import { anonKey, novelUrl, serviceKey, signedIn, todoUrl } from './client.mjs'
import { bin, root } from './server.mjs'

const servers = { novel: novelUrl, event: process.env.RULITE_EVENT_URL, todo: todoUrl }
const golden = (name) => readFileSync(join(root, 'sample', `${name}.types.ts`), 'utf8')
const types = (url, headers) => fetch(`${url}/pg/generators/typescript`, { headers })

describe('type export', () => {
  for (const [name, url] of Object.entries(servers)) {
    test(`service_role gets the types of ${name}`, async () => {
      const res = await types(url, { apikey: serviceKey })
      assert.equal(res.status, 200)
      assert.equal(res.headers.get('content-type'), 'text/plain; charset=utf-8')
      assert.equal(await res.text(), golden(name))
    })
  }

  test('anon is 401 42501, even with write on every table', async () => {
    const res = await types(novelUrl, { apikey: anonKey })
    assert.equal(res.status, 401)
    const body = await res.json()
    assert.equal(body.code, '42501')
    assert.match(body.message, /permission denied/)
  })

  test('a signed-in user is 403 42501', async () => {
    const alice = await signedIn('alice')
    const { data: { session } } = await alice.auth.getSession()
    const res = await types(todoUrl, { apikey: anonKey, Authorization: `Bearer ${session.access_token}` })
    assert.equal(res.status, 403)
    assert.equal((await res.json()).code, '42501')
  })

  test('no key is 401 PGRST302', async () => {
    const res = await types(novelUrl)
    assert.deepEqual([res.status, (await res.json()).code], [401, 'PGRST302'])
  })

  test('rulite gen types prints the same file offline', () => {
    const dir = mkdtempSync(join(tmpdir(), 'rulite-types-'))
    try {
      const db = new DatabaseSync(join(dir, 'event.db'))
      db.exec(readFileSync(join(root, 'sample', 'event.sql'), 'utf8'))
      db.close()
      const out = execFileSync(bin, ['gen', 'types', join(dir, 'event.db')], { encoding: 'utf8' })
      assert.equal(out, golden('event'))
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test('the generated types check supabase-js calls (tsc --noEmit)', () => {
    const tsc = join(import.meta.dirname, 'node_modules', '.bin', 'tsc')
    execFileSync(tsc, ['-p', join(import.meta.dirname, 'types', 'tsconfig.json')], { encoding: 'utf8' })
  })
})
