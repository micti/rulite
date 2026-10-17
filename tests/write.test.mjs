// T6.3: writes through supabase-js. Each test creates its own rows (unique titles),
// so the seed data that read.test.mjs relies on is left as it was.

import { describe, test } from 'node:test'
import assert from 'node:assert/strict'

import { event, novel, ok, unique } from './client.mjs'

/** Creates a novel and returns its id. */
async function createNovel(fields = {}) {
  const row = { title: unique('Novel'), author: 'Test Author', ...fields }
  return ok(await novel.from('novel').insert(row).select('id').single()).id
}

describe('insert', () => {
  test('single row, return=minimal', async () => {
    const title = unique('Minimal')
    const { data, error, status } = await novel.from('novel').insert({ title, author: 'A' })
    assert.equal(error, null)
    assert.equal(status, 201)
    assert.equal(data, null)
    assert.equal(ok(await novel.from('novel').select('status').eq('title', title).single()).status, 'ongoing')
  })

  test('single row with .select() returns the row, defaults filled in', async () => {
    const title = unique('Represented')
    const { data, error, status } = await novel.from('novel').insert({ title, author: 'B', genre: 'drama' }).select()
    assert.equal(error, null)
    assert.equal(status, 201)
    assert.equal(data.length, 1)
    assert.equal(data[0].title, title)
    assert.equal(data[0].status, 'ongoing')
    assert.match(data[0].id, /^[0-9a-f]{8}$/)
    assert.ok(data[0].created_at)
  })

  test('bulk insert in one request', async () => {
    const rows = [
      { title: unique('Bulk'), author: 'C', status: 'hiatus' },
      { title: unique('Bulk'), author: 'D', status: 'completed', genre: 'horror' },
    ]
    const data = ok(await novel.from('novel').insert(rows).select('title, genre, status'))
    assert.deepEqual(
      data.map((r) => [r.title, r.genre, r.status]),
      [
        [rows[0].title, null, 'hiatus'],
        [rows[1].title, 'horror', 'completed'],
      ],
    )
  })

  test('bulk insert: missing keys become null, or the default with defaultToNull: false', async () => {
    const rows = [{ title: unique('Bulk'), author: 'E', status: 'hiatus' }, { title: unique('Bulk'), author: 'F' }]
    const { error } = await novel.from('novel').insert(rows)
    assert.equal(error.code, '23502') // status is NOT NULL
    const data = ok(await novel.from('novel').insert(rows, { defaultToNull: false }).select('status'))
    assert.deepEqual(data.map((r) => r.status), ['hiatus', 'ongoing'])
  })

  test('select with an embed on the returned rows', async () => {
    const novelId = await createNovel()
    const data = ok(
      await novel.from('edition').insert({ novel_id: novelId, name: 'Paperback', language: 'fr' }).select('name, novel(author)').single(),
    )
    assert.deepEqual(data, { name: 'Paperback', novel: { author: 'Test Author' } })
  })

  test('JSON column round trip', async () => {
    const name = unique('Artist')
    const data = ok(await event.from('artist').insert({ name, genres: ['jazz', 'soul'] }).select('name, genres').single())
    assert.deepEqual(data, { name, genres: ['jazz', 'soul'] })
  })
})

describe('upsert', () => {
  test('merge-duplicates on a unique column updates the existing row', async () => {
    const title = unique('Upsert')
    const id = await createNovel({ title, genre: 'old' })
    const data = ok(
      await novel.from('novel').upsert({ title, author: 'Test Author', genre: 'new' }, { onConflict: 'title' }).select('id, genre'),
    )
    assert.deepEqual(data, [{ id, genre: 'new' }])
    const { count } = await novel.from('novel').select('*', { count: 'exact', head: true }).eq('title', title)
    assert.equal(count, 1)
  })

  test('upsert inserts rows that do not exist yet', async () => {
    const title = unique('Upsert')
    const data = ok(await novel.from('novel').upsert({ title, author: 'G' }, { onConflict: 'title' }).select('title'))
    assert.deepEqual(data, [{ title }])
  })

  test('upsert on a composite unique key', async () => {
    const novelId = await createNovel()
    const row = { novel_id: novelId, language: 'de', name: 'Erste' }
    ok(await novel.from('edition').upsert(row, { onConflict: 'novel_id,language' }))
    ok(await novel.from('edition').upsert({ ...row, name: 'Zweite' }, { onConflict: 'novel_id,language' }))
    const rows = ok(await novel.from('edition').select('name').eq('novel_id', novelId))
    assert.deepEqual(rows, [{ name: 'Zweite' }])
  })

  test('ignoreDuplicates keeps the existing row', async () => {
    const title = unique('Ignore')
    await createNovel({ title, genre: 'kept' })
    const data = ok(
      await novel
        .from('novel')
        .upsert({ title, author: 'H', genre: 'dropped' }, { onConflict: 'title', ignoreDuplicates: true })
        .select('genre'),
    )
    assert.deepEqual(data, [])
    assert.equal(ok(await novel.from('novel').select('genre').eq('title', title).single()).genre, 'kept')
  })

  test('onConflict without a matching unique key is a 400', async () => {
    const { error, status } = await novel.from('novel').upsert({ title: unique('X'), author: 'I' }, { onConflict: 'author' })
    assert.equal(status, 400)
    assert.equal(error.code, '42P10')
  })
})

describe('update and delete', () => {
  test('update with a filter', async () => {
    const id = await createNovel()
    const { data, error, status } = await novel.from('novel').update({ status: 'completed' }).eq('id', id)
    assert.equal(error, null)
    assert.equal(status, 204)
    assert.equal(data, null)
    assert.equal(ok(await novel.from('novel').select('status').eq('id', id).single()).status, 'completed')
  })

  test('update with .select() returns the updated rows', async () => {
    const author = unique('Author')
    const ids = [await createNovel({ author }), await createNovel({ author })]
    const data = ok(await novel.from('novel').update({ genre: 'updated' }).eq('author', author).select('id, genre'))
    assert.deepEqual(data.map((r) => r.id).sort(), ids.sort())
    assert.ok(data.every((r) => r.genre === 'updated'))
  })

  test('update with count', async () => {
    const author = unique('Author')
    await createNovel({ author })
    await createNovel({ author })
    const { count, error } = await novel.from('novel').update({ genre: 'counted' }, { count: 'exact' }).eq('author', author)
    assert.equal(error, null)
    assert.equal(count, 2)
  })

  test('delete with a filter, returning the deleted row', async () => {
    const title = unique('Delete')
    const id = await createNovel({ title })
    assert.deepEqual(ok(await novel.from('novel').delete().eq('id', id).select('title')), [{ title }])
    assert.equal(ok(await novel.from('novel').select('id').eq('id', id)).length, 0)
  })

  test('cascade: novel -> edition -> volume -> chapter -> part, then delete the novel', async () => {
    const novelId = await createNovel()
    const editionId = ok(
      await novel.from('edition').insert({ novel_id: novelId, name: 'Original', language: 'en' }).select('id').single(),
    ).id
    const volumeId = ok(
      await novel.from('volume').insert({ edition_id: editionId, number: 1, title: 'Book One' }).select('id').single(),
    ).id
    const chapterId = ok(
      await novel.from('chapter').insert({ volume_id: volumeId, number: 1, title: 'Start' }).select('id').single(),
    ).id
    ok(
      await novel.from('part').insert([
        { chapter_id: chapterId, number: 1, content: 'One.', word_count: 1 },
        { chapter_id: chapterId, number: 2, content: 'Two.', word_count: 1 },
      ]),
    )

    const tree = ok(
      await novel.from('novel').select('edition(volume(chapter(part(content))))').eq('id', novelId).single(),
    )
    assert.equal(tree.edition[0].volume[0].chapter[0].part.length, 2)

    ok(await novel.from('novel').delete().eq('id', novelId))
    for (const [table, column, id] of [
      ['edition', 'id', editionId],
      ['volume', 'id', volumeId],
      ['chapter', 'id', chapterId],
      ['part', 'chapter_id', chapterId],
    ]) {
      assert.deepEqual(ok(await novel.from(table).select('id').eq(column, id)), [], `${table} should be gone`)
    }
  })

  test('ON DELETE SET NULL', async () => {
    const area = ok(await event.from('area').insert({ name: unique('Stage'), kind: 'stage' }).select('id').single())
    const ev = ok(await event.from('event').select('id').eq('name', 'Winter Strings').single())
    const sub = ok(
      await event
        .from('sub_event')
        .insert({ event_id: ev.id, area_id: area.id, title: unique('Set'), starts_at: '2026-12-20 10:00', ends_at: '2026-12-20 11:00' })
        .select('id')
        .single(),
    )
    ok(await event.from('area').delete().eq('id', area.id))
    assert.equal(ok(await event.from('sub_event').select('area_id').eq('id', sub.id).single()).area_id, null)
    ok(await event.from('sub_event').delete().eq('id', sub.id))
  })
})

describe('errors', () => {
  test('unique violation is a 409 with 23505', async () => {
    const { error, status } = await novel.from('novel').insert({ title: 'The Lantern Road', author: 'Copycat' })
    assert.equal(status, 409)
    assert.equal(error.code, '23505')
    assert.ok(error.message)
    assert.ok('details' in error && 'hint' in error)
  })

  test('foreign key violation is a 409 with 23503', async () => {
    const { error, status } = await novel.from('edition').insert({ novel_id: 'deadbeef', name: 'Ghost', language: 'xx' })
    assert.equal(status, 409)
    assert.equal(error.code, '23503')
  })

  test('not-null violation is a 400 with 23502', async () => {
    const { error, status } = await novel.from('novel').insert({ title: unique('No author') })
    assert.equal(status, 400)
    assert.equal(error.code, '23502')
  })

  test('check violation is a 400 with 23514', async () => {
    const { error, status } = await novel.from('novel').insert({ title: unique('Bad status'), author: 'J', status: 'lost' })
    assert.equal(status, 400)
    assert.equal(error.code, '23514')
  })

  test('unknown column in the body', async () => {
    const { error, status } = await novel.from('novel').insert({ title: unique('X'), author: 'K', nope: 1 })
    assert.equal(status, 400)
    assert.equal(error.code, 'PGRST204')
  })

  test('a failed bulk insert writes nothing', async () => {
    const title = unique('Atomic')
    const { error } = await novel.from('novel').insert([
      { title, author: 'L' },
      { title: 'The Lantern Road', author: 'M' },
    ])
    assert.equal(error.code, '23505')
    assert.deepEqual(ok(await novel.from('novel').select('id').eq('title', title)), [])
  })

  test('update without a filter is refused', async () => {
    const { error, status } = await novel.from('novel').update({ genre: 'everything' })
    assert.equal(status, 400)
    assert.equal(error.code, '21000')
    assert.equal(ok(await novel.from('novel').select('id').eq('genre', 'everything')).length, 0)
  })

  test('delete without a filter is refused', async () => {
    const { error, status } = await novel.from('part').delete()
    assert.equal(status, 400)
    assert.equal(error.code, '21000')
    const { count } = await novel.from('part').select('*', { count: 'exact', head: true })
    assert.ok(count >= 10)
  })

  test('single() on a write that touches several rows rolls back', async () => {
    const author = unique('Author')
    await createNovel({ author })
    await createNovel({ author })
    const { error, status } = await novel.from('novel').update({ genre: 'single' }).eq('author', author).select().single()
    assert.equal(status, 406)
    assert.equal(error.code, 'PGRST116')
    assert.equal(ok(await novel.from('novel').select('id').eq('author', author).eq('genre', 'single')).length, 0)
  })
})
