// M20-M25: views and functions (`/rest/v1/rpc/{name}`) through supabase-js.
//   novel: sample/functions/novel, every function open to anon ("*")
//   todo:  sample/functions/todo, authenticated only, whoami also anon

import { before, describe, test } from 'node:test'
import assert from 'node:assert/strict'

import { novel, novelUrl, anonKey, ok, serviceKey, signedIn, todo, unique } from './client.mjs'

describe('views', () => {
  test('a view reads like a table', async () => {
    const rows = ok(await novel.from('novel_stats').select('title, editions').gte('editions', 2))
    assert.deepEqual(rows, [{ title: 'The Lantern Road', editions: 2 }])
    const { data, count } = await novel.from('novel_stats').select('*', { count: 'exact' }).eq('status', 'hiatus')
    assert.equal(count, 1)
    assert.deepEqual(Object.keys(data[0]).sort(), ['editions', 'id', 'status', 'title'])
  })

  test('a view is read-only: 405 55000', async () => {
    for (const { error, status } of [
      await novel.from('novel_stats').insert({ title: 'x' }),
      await novel.from('novel_stats').update({ title: 'x' }).eq('title', 'nope'),
      await novel.from('novel_stats').delete().eq('title', 'nope'),
    ]) {
      assert.equal(status, 405)
      assert.equal(error.code, '55000')
      assert.match(error.message, /view "novel_stats"/)
    }
  })
})

describe('functions that read', () => {
  test('setof: arguments in the body, filters and order on the result', async () => {
    const rows = ok(await novel.rpc('search_novels', { q: 'a' }).neq('status', 'hiatus').order('title'))
    assert.deepEqual(
      rows.map((r) => [r.title, r.editions]),
      [
        ['Signal Drift', 1],
        ['The Lantern Road', 2],
      ],
    )
    const picked = ok(await novel.rpc('search_novels', { q: 'Road' }).select('title, n:editions'))
    assert.deepEqual(picked, [{ title: 'The Lantern Road', n: 2 }])
  })

  test('count, paging, GET and HEAD', async () => {
    const all = ok(await novel.from('novel').select('id'))
    const paged = await novel.rpc('search_novels', { q: '' }, { count: 'exact' }).order('title').range(0, 0)
    assert.equal(paged.count, all.length)
    assert.equal(paged.data.length, 1)
    assert.equal(paged.status, 206)

    const get = await novel.rpc('search_novels', { q: 'Harbor', max: 5 }, { get: true }).select('title')
    assert.deepEqual(ok(get), [{ title: 'Quiet Harbor' }])
    const head = await novel.rpc('search_novels', { q: '' }, { head: true, count: 'exact' })
    assert.equal(head.data, null)
    assert.equal(head.count, all.length)
  })

  test('single(), and an argument default', async () => {
    const one = ok(await novel.rpc('search_novels', { q: 'Drift' }).single())
    assert.equal(one.title, 'Signal Drift')
    const many = await novel.rpc('search_novels', { q: '' }).single()
    assert.equal(many.error.code, 'PGRST116')
    assert.equal(ok(await novel.rpc('search_novels', { q: '', max: 1 })).length, 1)
  })

  test('value, json and the arguments of GET', async () => {
    const total = ok(await novel.from('novel').select('id')).length
    assert.equal(ok(await novel.rpc('novel_count')), total)
    assert.equal(ok(await novel.rpc('novel_count', { status: 'hiatus' })), 1)
    assert.equal(ok(await novel.rpc('novel_count', { status: 'hiatus' }, { get: true })), 1)
    const summary = ok(await novel.rpc('novel_summary'))
    assert.equal(summary.novels, total)
    assert.equal(summary.by_status.hiatus, 1)
    const { error, status } = await novel.rpc('novel_count').eq('status', 'x')
    assert.equal(status, 400)
    assert.equal(error.code, 'PGRST100')
  })

  test('unknown functions and arguments are PGRST202, bad values 22P02', async () => {
    const missing = await novel.rpc('nope', { a: 1 })
    assert.equal(missing.status, 404)
    assert.equal(missing.error.code, 'PGRST202')
    assert.equal(missing.error.message, 'Could not find the function public.nope(a) in the schema cache')

    for (const args of [{ q: 'a', limit: 1 }, {}]) {
      const { error, status } = await novel.rpc('search_novels', args)
      assert.equal(status, 404)
      assert.equal(error.code, 'PGRST202')
      assert.equal(error.hint, 'Perhaps you meant to call the function public.search_novels(max, q)')
    }
    const bad = await novel.rpc('search_novels', { q: 'a', max: 'lots' })
    assert.equal(bad.status, 400)
    assert.equal(bad.error.code, '22P02')
    assert.equal(bad.error.message, 'invalid input syntax for type integer: "lots"')
  })

  test('a filter on a column the result lacks is 42703', async () => {
    const { error } = await novel.rpc('search_novels', { q: '' }).eq('genre', 'x')
    assert.equal(error.code, '42703')
  })
})

describe('functions that write', () => {
  test('row: one transaction on the writer; a failure rolls back', async () => {
    const title = unique('Rpc Novel')
    const row = ok(await novel.rpc('add_novel', { title, author: 'Rulite' }))
    assert.deepEqual([row.title, row.author, row.genre, row.status], [title, 'Rulite', null, 'ongoing'])
    assert.equal(row.id.length, 8)

    const again = await novel.rpc('add_novel', { title, author: 'Other' })
    assert.equal(again.status, 409)
    assert.equal(again.error.code, '23505')
    assert.equal(ok(await novel.from('novel').select('author').eq('title', title)).length, 1)
  })

  test('GET on a function that writes is 405 25006; filters are 400', async () => {
    const get = await novel.rpc('add_novel', { title: 'x', author: 'y' }, { get: true })
    assert.equal(get.status, 405)
    assert.equal(get.error.code, '25006')
    const filtered = await novel.rpc('add_novel', { title: unique('F'), author: 'y' }).eq('title', 'x')
    assert.equal(filtered.status, 400)
  })

  test('the raw endpoint answers like PostgREST', async () => {
    const res = await fetch(`${novelUrl}/rest/v1/rpc/novel_count?status=hiatus`, { headers: { apikey: anonKey } })
    assert.equal(res.status, 200)
    assert.equal(await res.text(), '1')
    const post = await fetch(`${novelUrl}/rest/v1/rpc/novel_count`, {
      method: 'POST',
      headers: { apikey: anonKey, 'Content-Type': 'application/json' },
      body: '[1]',
    })
    assert.deepEqual([post.status, (await post.json()).code], [400, 'PGRST102'])
  })
})

describe('roles and :auth_uid', () => {
  let alice, bob, aliceId

  before(async () => {
    alice = await signedIn('alice')
    bob = await signedIn('bob')
    aliceId = ok(await alice.auth.getUser()).user.id
  })

  test(':auth_uid and :auth_role are bound for every role', async () => {
    assert.deepEqual(ok(await todo().rpc('whoami')), { id: null, role: 'anon' })
    assert.deepEqual(ok(await alice.rpc('whoami')), { id: aliceId, role: 'authenticated' })
    assert.deepEqual(ok(await todo(serviceKey).rpc('whoami')), { id: null, role: 'service_role' })
  })

  test('an ungranted function is 401 for anon', async () => {
    const { error, status } = await todo().rpc('my_open_todos')
    assert.equal(status, 401)
    assert.equal(error.code, '42501')
    assert.equal(error.message, 'permission denied for function my_open_todos')
  })

  test('functions limit rows with :auth_uid', async () => {
    const list = ok(await alice.from('list').insert({ name: unique('Rpc list') }).select().single())
    const title = unique('Rpc todo')
    const added = ok(await alice.rpc('add_todo', { list_id: list.id, title }))
    assert.deepEqual([added.list_id, added.title, added.done], [list.id, title, 0])
    // Not bob's list: nothing is inserted.
    assert.equal(ok(await bob.rpc('add_todo', { list_id: list.id, title: 'intruder' })), null)

    const mine = ok(await alice.rpc('my_open_todos').eq('list', list.name))
    assert.deepEqual(mine.map((t) => t.title), [title])
    assert.deepEqual(ok(await bob.rpc('my_open_todos').eq('list', list.name)), [])

    ok(await alice.from('todo').update({ done: 1 }).eq('id', added.id))
    const cleared = await alice.rpc('clear_done', { drop_empty: true })
    assert.deepEqual([cleared.status, cleared.data, cleared.error], [204, null, null])
    assert.deepEqual(ok(await alice.from('todo').select('id').eq('id', added.id)), [])
    assert.deepEqual(ok(await alice.from('list').select('id').eq('id', list.id)), [])
  })
})
