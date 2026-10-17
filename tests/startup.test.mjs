// M13: every server runs with auth; a role sees only the tables its config grants.

import { after, describe, test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { createClient } from '@supabase/supabase-js'

import { anonKey, novelUrl, secret, serviceKey } from './client.mjs'
import { exited, serverEnv, start } from './server.mjs'

const dir = mkdtempSync(join(tmpdir(), 'rulite-startup-'))
const children = []
after(() => {
  for (const child of children) child.kill('SIGTERM')
  rmSync(dir, { recursive: true, force: true })
})

/** A database with the tables `open` and `closed`, and a config file with `tables`. */
function setup(name, tables) {
  const db = new DatabaseSync(join(dir, `${name}.db`))
  db.exec("CREATE TABLE open (id INTEGER PRIMARY KEY, name TEXT); CREATE TABLE closed (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO open (name) VALUES ('a'); INSERT INTO closed (name) VALUES ('b');")
  db.close()
  const config = join(dir, `${name}.json`)
  writeFileSync(config, JSON.stringify({ jwt_secret: secret, tables }))
  return [join(dir, `${name}.db`), config]
}

async function serve(args, options) {
  const server = await start(args, options)
  children.push(server.child)
  return server
}

describe('auth is always on', () => {
  test('no jwt_secret: rulite refuses to start', async () => {
    const [db] = setup('nosecret', {})
    const { status, stderr } = await exited([db])
    assert.equal(status, 2)
    assert.match(stderr, /^rulite: a jwt_secret is required \(--config or RULITE_JWT_SECRET\); see AUTH\.md/)
  })

  test('RULITE_JWT_SECRET is enough to start; only service_role works then', async () => {
    const [db] = setup('envsecret', {})
    const { url, log } = await serve([db], { env: serverEnv({ RULITE_JWT_SECRET: secret }) })
    assert.match(log(), /warning: no table is granted to anon or authenticated/)
    const anon = createClient(url, anonKey)
    assert.equal((await anon.from('open').select()).error.code, '42501')
    const service = createClient(url, serviceKey)
    assert.equal((await service.from('open').select()).data.length, 1)
  })

  test('novel without a key is 401 PGRST302', async () => {
    const res = await fetch(`${novelUrl}/rest/v1/novel`)
    assert.equal(res.status, 401)
    assert.equal((await res.json()).code, 'PGRST302')
    const doc = await fetch(`${novelUrl}/rest/v1/`, { headers: { apikey: anonKey } })
    assert.deepEqual([doc.status, (await doc.json()).code], [401, '42501'])
  })

  test('a table left out of the config is closed', async () => {
    const [db, config] = setup('partial', { open: { anon: 'read' } })
    const { url } = await serve([db, '--config', config])
    const anon = createClient(url, anonKey)
    assert.deepEqual((await anon.from('open').select('name')).data, [{ name: 'a' }])
    const { error, status } = await anon.from('closed').select()
    assert.equal(error.code, '42501')
    assert.equal(status, 401)
    assert.equal((await createClient(url, serviceKey).from('closed').select('name')).data[0].name, 'b')
  })
})
