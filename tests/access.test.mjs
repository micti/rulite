// T12.4: roles, table permissions and owner rows on the todo server (sample/todo.json):
//   tag      anon + authenticated read
//   list     authenticated write, owner owner_id
//   todo     authenticated write, owner user_id
//   comment  anon read, authenticated write, owner author_id, owner_scope "write"
//   audit    not granted: closed

import { before, describe, test } from 'node:test'
import assert from 'node:assert/strict'

import { anonKey, ok, serviceKey, sign, signedIn, todo, todoUrl, unique } from './client.mjs'

const anon = todo()
const service = todo(serviceKey)
let alice, bob, aliceId, bobId

before(async () => {
  alice = await signedIn('alice')
  bob = await signedIn('bob')
  aliceId = ok(await alice.auth.getUser()).user.id
  bobId = ok(await bob.auth.getUser()).user.id
})

/** Asserts a supabase-js result failed with `42501`. */
function denied({ error, data }) {
  assert.equal(error?.code, '42501', JSON.stringify(error))
  assert.match(error.message, /permission denied|row-level security/)
  assert.equal(data, null)
}

async function createList(client, name = unique('List')) {
  return ok(await client.from('list').insert({ name }).select().single())
}

describe('keys and tokens', () => {
  test('no key is PGRST302', async () => {
    const res = await fetch(`${todoUrl}/rest/v1/tag`)
    assert.equal(res.status, 401)
    assert.equal((await res.json()).code, 'PGRST302')
    // The role is checked before the body is read.
    const post = await fetch(`${todoUrl}/rest/v1/tag`, { method: 'POST', body: 'not json' })
    assert.deepEqual([post.status, (await post.json()).code], [401, 'PGRST302'])
  })

  test('a tampered or foreign token is PGRST301', async () => {
    const [head, , sig] = anonKey.split('.')
    const forged = `${head}.${Buffer.from('{"role":"service_role"}').toString('base64url')}.${sig}`
    for (const token of [forged, sign({ role: 'service_role' }, 'another-secret-of-at-least-32-bytes'), 'not-a-jwt']) {
      const { error, status } = await todo(anonKey, { Authorization: `Bearer ${token}` }).from('tag').select()
      assert.equal(error.code, 'PGRST301')
      assert.equal(status, 401)
    }
    const none = sign({ role: 'service_role' }, 'x', { alg: 'none', typ: 'JWT' }).replace(/[^.]+$/, '')
    const { error } = await todo(none).from('tag').select()
    assert.equal(error.code, 'PGRST301')
  })

  test('an expired token is PGRST303', async () => {
    const expired = sign({ sub: aliceId, role: 'authenticated', exp: Math.floor(Date.now() / 1000) - 120 })
    const { error, status } = await todo(anonKey, { Authorization: `Bearer ${expired}` }).from('tag').select()
    assert.equal(error.code, 'PGRST303')
    assert.equal(status, 401)
  })

  test('OpenAPI is for service_role only', async () => {
    const get = (headers) => fetch(`${todoUrl}/rest/v1/`, { headers })
    const asAnon = await get({ apikey: anonKey })
    assert.equal(asAnon.status, 401)
    assert.equal((await asAnon.json()).code, '42501')
    const { data: { session } } = await alice.auth.getSession()
    const asAlice = await get({ apikey: anonKey, Authorization: `Bearer ${session.access_token}` })
    assert.equal(asAlice.status, 403)
    const asService = await get({ apikey: serviceKey })
    assert.equal(asService.status, 200)
    const doc = await asService.json()
    assert.ok(doc.paths['/list'])
    assert.equal(doc.paths['/auth_users'], undefined)
  })
})

describe('table permissions', () => {
  test('anon reads a read table but cannot write it', async () => {
    const tags = ok(await anon.from('tag').select('name').order('name'))
    assert.deepEqual(tags.map((t) => t.name), ['home', 'urgent', 'work'])
    const res = await anon.from('tag').insert({ name: unique('tag') })
    denied(res)
    assert.equal(res.status, 401)
  })

  test('authenticated reads but cannot write a read table', async () => {
    ok(await alice.from('tag').select())
    const res = await alice.from('tag').update({ name: 'x' }).eq('name', 'home')
    denied(res)
    assert.equal(res.status, 403)
  })

  test('closed tables', async () => {
    for (const [client, status] of [[anon, 401], [alice, 403]]) {
      const res = await client.from('audit').select()
      denied(res)
      assert.equal(res.status, status)
    }
    denied(await anon.from('list').select())
    denied(await anon.from('todo').insert({ title: 'x' }))
  })

  test('an embed of a closed table is denied', async () => {
    denied(await alice.from('tag').select('name, audit(note)'))
    denied(await alice.from('tag').select('name, audit!inner()'))
  })

  test('the auth tables are not exposed', async () => {
    const { error, status } = await service.from('auth_users').select()
    assert.equal(error.code, 'PGRST205')
    assert.equal(status, 404)
  })

  test('service_role reads and writes everything', async () => {
    assert.equal(ok(await service.from('audit').select()).length >= 1, true)
    const tag = ok(await service.from('tag').insert({ name: unique('tag') }).select().single())
    ok(await service.from('tag').delete().eq('id', tag.id))
  })
})

describe('owner rows', () => {
  test('insert fills the owner; select shows own rows only', async () => {
    const mine = await createList(alice)
    assert.equal(mine.owner_id, aliceId)
    const theirs = await createList(bob)
    const lists = ok(await alice.from('list').select('id, owner_id'))
    assert.ok(lists.some((l) => l.id === mine.id))
    assert.ok(lists.every((l) => l.owner_id === aliceId))
    assert.ok(!lists.some((l) => l.id === theirs.id))
    const { count } = await alice.from('list').select('*', { count: 'exact', head: true })
    assert.equal(count, lists.length)
  })

  test('insert for another user is refused and nothing is written', async () => {
    const name = unique('Not mine')
    const res = await alice.from('list').insert([{ name: unique('ok') }, { name, owner_id: bobId }])
    denied(res)
    assert.equal(res.status, 403)
    assert.match(res.error.message, /row-level security policy for table "list"/)
    assert.deepEqual(ok(await service.from('list').select().eq('name', name)), [])
  })

  test('update and delete skip other users\' rows', async () => {
    const theirs = await createList(bob)
    assert.deepEqual(ok(await alice.from('list').update({ name: 'taken' }).eq('id', theirs.id).select()), [])
    assert.deepEqual(ok(await alice.from('list').delete().eq('id', theirs.id).select()), [])
    const still = ok(await service.from('list').select('name').eq('id', theirs.id).single())
    assert.equal(still.name, theirs.name)
  })

  test('changing the owner to another user is refused', async () => {
    const mine = await createList(alice)
    denied(await alice.from('list').update({ owner_id: bobId }).eq('id', mine.id))
    assert.equal(ok(await service.from('list').select('owner_id').eq('id', mine.id).single()).owner_id, aliceId)
  })

  test('upsert onto another user\'s row is refused', async () => {
    const theirs = await createList(bob)
    denied(await alice.from('list').upsert({ id: theirs.id, name: 'mine now' }))
    assert.equal(ok(await service.from('list').select('name').eq('id', theirs.id).single()).name, theirs.name)
    const mine = await createList(alice)
    const updated = ok(await alice.from('list').upsert({ id: mine.id, name: 'renamed' }).select().single())
    assert.equal(updated.name, 'renamed')
  })

  test('embeds, !inner and count hide other users\' rows', async () => {
    const list = await createList(alice)
    ok(await alice.from('todo').insert({ list_id: list.id, title: 'alice todo' }))
    // bob may add his own todo to any list id he knows; alice still doesn't see it.
    ok(await bob.from('todo').insert({ list_id: list.id, title: 'bob todo' }))
    const tree = ok(await alice.from('list').select('id, todo(title)').eq('id', list.id).single())
    assert.deepEqual(tree.todo, [{ title: 'alice todo' }])
    const bobs = ok(await bob.from('todo').select('title, list(id)').eq('list_id', list.id))
    assert.deepEqual(bobs, [{ title: 'bob todo', list: null }])
    const { data, count } = await bob.from('todo').select('title, list!inner(id)', { count: 'exact' }).eq('list_id', list.id)
    assert.deepEqual([data, count], [[], 0])
    const all = ok(await service.from('list').select('todo(title)').eq('id', list.id).single())
    assert.equal(all.todo.length, 2)
  })

  test('delete removes own rows only', async () => {
    const mine = await createList(alice)
    const theirs = await createList(bob)
    const gone = ok(await alice.from('list').delete().in('id', [mine.id, theirs.id]).select('id'))
    assert.deepEqual(gone, [{ id: mine.id }])
    assert.ok(ok(await service.from('list').select('id').eq('id', theirs.id).single()))
  })

  test('owner_scope "write": everyone reads, authors edit their own', async () => {
    const tagId = ok(await anon.from('tag').select('id').eq('name', 'home').single()).id
    const aliceComment = ok(await alice.from('comment').insert({ tag_id: tagId, body: unique('hi') }).select().single())
    const bobComment = ok(await bob.from('comment').insert({ tag_id: tagId, body: unique('yo') }).select().single())
    for (const client of [anon, alice]) {
      const ids = ok(await client.from('comment').select('id')).map((c) => c.id)
      assert.ok(ids.includes(aliceComment.id) && ids.includes(bobComment.id))
    }
    const embedded = ok(await anon.from('tag').select('name, comment(body)').eq('id', tagId).single())
    assert.ok(embedded.comment.length >= 2)
    assert.deepEqual(ok(await alice.from('comment').update({ body: 'edited' }).eq('id', bobComment.id).select()), [])
    assert.equal(ok(await alice.from('comment').update({ body: 'edited' }).eq('id', aliceComment.id).select().single()).body, 'edited')
    denied(await anon.from('comment').delete().eq('id', aliceComment.id))
  })

  test('service_role sees every row and writes for anyone', async () => {
    const forBob = ok(await service.from('list').insert({ name: unique('seeded'), owner_id: bobId }).select().single())
    assert.equal(forBob.owner_id, bobId)
    assert.ok(ok(await bob.from('list').select('id')).some((l) => l.id === forBob.id))
    const owners = new Set(ok(await service.from('list').select('owner_id')).map((l) => l.owner_id))
    assert.ok(owners.has(aliceId) && owners.has(bobId))
  })
})
