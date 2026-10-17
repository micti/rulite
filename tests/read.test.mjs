// T6.2: reads through supabase-js on fresh copies of the sample databases.

import { describe, test } from 'node:test'
import assert from 'node:assert/strict'

import { event, novel, ok } from './client.mjs'

const titles = (rows) => rows.map((r) => r.title)

describe('filters', () => {
  test('eq', async () => {
    const rows = ok(await novel.from('novel').select('id, title').eq('status', 'completed'))
    assert.deepEqual(titles(rows), ['The Lantern Road'])
    assert.match(rows[0].id, /^[0-9a-f]{8}$/)
  })

  test('gte on integers', async () => {
    const rows = ok(await novel.from('part').select('word_count').gte('word_count', 9).order('word_count'))
    assert.deepEqual(rows.map((r) => r.word_count), [9, 9, 10, 10])
  })

  test('in, with a value that needs quoting', async () => {
    const rows = ok(await novel.from('novel').select('title').in('status', ['completed', 'hiatus', 'a,b']).order('title'))
    assert.deepEqual(titles(rows), ['Quiet Harbor', 'The Lantern Road'])
  })

  test('is null / not is null', async () => {
    assert.deepEqual(ok(await novel.from('novel').select('title').is('genre', null)), [])
    const rows = ok(await novel.from('novel').select('title').not('genre', 'is', null))
    assert.equal(rows.length, 3)
  })

  test('like / ilike', async () => {
    assert.deepEqual(titles(ok(await novel.from('novel').select('title').like('title', '%Road'))), ['The Lantern Road'])
    assert.deepEqual(titles(ok(await novel.from('novel').select('title').like('title', '%road'))), [])
    assert.deepEqual(titles(ok(await novel.from('novel').select('title').ilike('title', '%SIGNAL%'))), ['Signal Drift'])
  })

  test('or', async () => {
    const rows = ok(await novel.from('novel').select('title').or('genre.eq.fantasy,status.eq.hiatus').order('title'))
    assert.deepEqual(titles(rows), ['Quiet Harbor', 'The Lantern Road'])
  })

  test('not', async () => {
    const rows = ok(await novel.from('novel').select('title').not('status', 'eq', 'completed').order('title'))
    assert.deepEqual(titles(rows), ['Quiet Harbor', 'Signal Drift'])
  })

  test('or with nested and / not.and', async () => {
    const rows = ok(
      await novel
        .from('novel')
        .select('title')
        .or('and(genre.eq.sci-fi,author.like.Tomas*),not.and(status.neq.hiatus,genre.neq.mystery)')
        .order('title'),
    )
    assert.deepEqual(titles(rows), ['Quiet Harbor', 'Signal Drift'])
  })

  test('unknown column is a 400 with 42703', async () => {
    const { error, status } = await novel.from('novel').select('title').eq('nope', 1)
    assert.equal(status, 400)
    assert.equal(error.code, '42703')
  })

  test('unknown table is a 404 with PGRST205', async () => {
    const { error, status } = await novel.from('nope').select('*')
    assert.equal(status, 404)
    assert.equal(error.code, 'PGRST205')
  })
})

describe('order and paging', () => {
  test('order asc / desc, multi-column', async () => {
    assert.deepEqual(titles(ok(await novel.from('novel').select('title').order('title'))), [
      'Quiet Harbor',
      'Signal Drift',
      'The Lantern Road',
    ])
    const rows = ok(
      await novel.from('chapter').select('number, title').order('number', { ascending: false }).order('title').limit(3),
    )
    assert.deepEqual(rows, [
      { number: 2, title: 'Echoes' },
      { number: 2, title: 'The Toll Keeper' },
      { number: 1, title: 'A Light in the Fog' },
    ])
  })

  test('nulls first / last', async () => {
    const first = ok(await event.from('sub_event').select('price').order('price', { nullsFirst: true }))
    assert.equal(first[0].price, null)
    const last = ok(await event.from('sub_event').select('price').order('price', { nullsFirst: false }))
    assert.equal(last.at(-1).price, null)
  })

  test('limit', async () => {
    assert.equal(ok(await novel.from('part').select('id').limit(4)).length, 4)
  })

  test('range', async () => {
    const rows = ok(await novel.from('novel').select('title').order('title').range(1, 2))
    assert.deepEqual(titles(rows), ['Signal Drift', 'The Lantern Road'])
  })
})

describe('embedding', () => {
  test('novel -> edition -> volume -> chapter -> part', async () => {
    const data = ok(
      await novel
        .from('novel')
        .select('title, edition(language, volume(number, chapter(title, part(content))))')
        .eq('title', 'The Lantern Road')
        .order('language', { referencedTable: 'edition' })
        .order('number', { referencedTable: 'edition.volume' })
        .single(),
    )
    assert.equal(data.edition.length, 2)
    const [en, vi] = data.edition
    assert.deepEqual([en.language, vi.language], ['en', 'vi'])
    assert.deepEqual(en.volume.map((v) => v.number), [1, 2])
    assert.equal(en.volume[1].chapter[0].title, 'Ashes at Dawn')
    assert.deepEqual(en.volume[1].chapter[0].part, [{ content: 'By morning the village was smoke.' }])
    assert.equal(vi.volume[0].chapter[0].part[0].content, 'Con đường không có điểm cuối mà ai còn nhớ.')
  })

  test('part -> chapter -> volume (many-to-one)', async () => {
    const rows = ok(
      await novel
        .from('part')
        .select('content, chapter(title, volume(title))')
        .gte('word_count', 8)
        .eq('content', 'Then a lantern swung out of the fog.'),
    )
    assert.deepEqual(rows, [
      { content: 'Then a lantern swung out of the fog.', chapter: { title: 'A Light in the Fog', volume: { title: 'The Dark Mile' } } },
    ])
  })

  test('filter on an embedded resource', async () => {
    const data = ok(
      await novel.from('novel').select('title, edition(language)').eq('edition.language', 'vi').order('title'),
    )
    assert.equal(data.length, 3)
    assert.deepEqual(data.find((n) => n.title === 'The Lantern Road').edition, [{ language: 'vi' }])
    assert.deepEqual(data.find((n) => n.title === 'Signal Drift').edition, [])
  })

  test('!inner drops parents without a match', async () => {
    const data = ok(await novel.from('novel').select('title, edition!inner(language)').eq('edition.language', 'vi'))
    assert.deepEqual(data, [{ title: 'The Lantern Road', edition: [{ language: 'vi' }] }])
  })

  test('limit on an embedded resource', async () => {
    const data = ok(
      await novel
        .from('novel')
        .select('title, edition(language)')
        .eq('title', 'The Lantern Road')
        .order('language', { referencedTable: 'edition', ascending: false })
        .limit(1, { referencedTable: 'edition' })
        .single(),
    )
    assert.deepEqual(data.edition, [{ language: 'vi' }])
  })

  test('self reference with alias and hint, many-to-many, JSON column', async () => {
    const data = ok(
      await event
        .from('event')
        .select('name, area(name, parent:area!parent_id(name)), sub_event(title, area(name), artist(name, genres))')
        .eq('name', 'Saigon Sound Fest 2026')
        .order('starts_at', { referencedTable: 'sub_event' })
        .order('name', { referencedTable: 'sub_event.artist' })
        .single(),
    )
    assert.deepEqual(data.area, { name: 'Riverside Park', parent: { name: 'Ho Chi Minh City' } })
    assert.deepEqual(titles(data.sub_event), ['Acoustic Sunset', 'Opening Night', 'Closing Party'])
    const opening = data.sub_event[1]
    assert.deepEqual(opening.area, { name: 'Main Stage' })
    assert.deepEqual(opening.artist.map((a) => a.name), ['DJ Nova', 'Kai Lam', 'Thu Ha'])
    assert.deepEqual(opening.artist[2].genres, ['pop', 'ballad'])
  })

  test('one-to-many self reference (children)', async () => {
    const rows = ok(
      await event
        .from('area')
        .select('name, children:area!parent_id!many(name, capacity)')
        .eq('kind', 'venue')
        .order('name')
        .order('name', { referencedTable: 'children' }),
    )
    assert.deepEqual(rows, [
      { name: 'Beach Arena', children: [{ name: 'Sand Stage', capacity: 8000 }] },
      { name: 'Opera House', children: [{ name: 'Grand Hall', capacity: 500 }] },
      {
        name: 'Riverside Park',
        children: [
          { name: 'Garden Stage', capacity: 3000 },
          { name: 'Main Stage', capacity: 15000 },
        ],
      },
    ])
  })

  test('many-to-many from the other side, empty embeds', async () => {
    const rows = ok(
      await event.from('artist').select('name, sub_event(title, event(name))').eq('country', 'VN').order('name'),
    )
    assert.deepEqual(rows.map((r) => r.name), ['Kai Lam', 'Lotus Quartet', 'Thu Ha'])
    assert.deepEqual(rows[1].sub_event, [{ title: 'Quartet Evening', event: { name: 'Winter Strings' } }])
    const unbooked = ok(await event.from('artist').select('name, sub_event(title)').eq('country', 'JP').single())
    assert.deepEqual(unbooked.sub_event, [])
  })

  test('nullable many-to-one embeds null', async () => {
    const data = ok(await event.from('sub_event').select('title, area(name)').eq('title', 'Beach Cleanup').single())
    assert.deepEqual(data, { title: 'Beach Cleanup', area: null })
  })

  test('ambiguous embed asks for a hint', async () => {
    const { error, status } = await event.from('area').select('name, area(name)')
    assert.equal(status, 300)
    assert.equal(error.code, 'PGRST201')
  })

  test('unknown relationship', async () => {
    const { error, status } = await novel.from('novel').select('title, chapter(title)')
    assert.equal(status, 400)
    assert.equal(error.code, 'PGRST200')
  })
})

describe('single and count', () => {
  test('single() returns an object', async () => {
    const { data, error, status } = await novel.from('novel').select('title, status').eq('title', 'Quiet Harbor').single()
    assert.equal(error, null)
    assert.equal(status, 200)
    assert.deepEqual(data, { title: 'Quiet Harbor', status: 'hiatus' })
  })

  test('single() with several or no rows is a 406', async () => {
    for (const query of [novel.from('novel').select('title'), novel.from('novel').select('title').eq('title', 'nope')]) {
      const { data, error, status } = await query.single()
      assert.equal(data, null)
      assert.equal(status, 406)
      assert.equal(error.code, 'PGRST116')
    }
  })

  test('maybeSingle()', async () => {
    assert.deepEqual(ok(await novel.from('novel').select('title').eq('title', 'Signal Drift').maybeSingle()), {
      title: 'Signal Drift',
    })
    const { data, error } = await novel.from('novel').select('title').eq('title', 'nope').maybeSingle()
    assert.equal(data, null)
    assert.equal(error, null)
  })

  test('count exact with a partial page is a 206', async () => {
    const { data, count, status } = await novel.from('part').select('id', { count: 'exact' }).limit(3)
    assert.equal(status, 206)
    assert.equal(count, 10)
    assert.equal(data.length, 3)
  })

  test('count exact covering every row is a 200', async () => {
    const { data, count, status } = await novel.from('novel').select('id', { count: 'exact' }).eq('genre', 'sci-fi')
    assert.equal(status, 200)
    assert.equal(count, 1)
    assert.equal(data.length, 1)
  })

  test('count with head: true (HEAD request)', async () => {
    const { data, count, error } = await novel.from('chapter').select('*', { count: 'exact', head: true })
    assert.equal(error, null)
    assert.equal(data, null)
    assert.equal(count, 7)
  })
})

describe('types', () => {
  test('integers, reals, nulls, 0/1 booleans', async () => {
    const data = ok(
      await event.from('sub_event').select('price, event(is_public)').eq('title', 'Sunrise Session').single(),
    )
    assert.deepEqual(data, { price: null, event: { is_public: 0 } })
    const opening = ok(await event.from('sub_event').select('price').eq('title', 'Opening Night').single())
    assert.equal(opening.price, 45)
  })
})
