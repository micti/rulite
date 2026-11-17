// M16: the generated types (sample/*.types.ts, compared with what the servers serve) give
// supabase-js typed from(), select() with embeds, insert() and update(). Checked by `tsc --noEmit`.

import { createClient } from '@supabase/supabase-js'

import type { Database as Event } from '../../sample/event.types'
import type { Database as Novel, Tables, TablesInsert } from '../../sample/novel.types'
import type { Database as Todo } from '../../sample/todo.types'

const novel = createClient<Novel>('http://127.0.0.1:3000', 'key')
const event = createClient<Event>('http://127.0.0.1:3000', 'key')
const todo = createClient<Todo>('http://127.0.0.1:3000', 'key')

export async function reads() {
  const { data } = await novel
    .from('novel')
    .select('id, title, genre, edition(language, volume(number, chapter(title)))')
    .eq('status', 'completed')
  if (!data) return
  const title: string = data[0].title
  const genre: string | null = data[0].genre
  const volume: number = data[0].edition[0].volume[0].number
  const chapter: string = data[0].edition[0].volume[0].chapter[0].title
  // @ts-expect-error: no such column
  data[0].nope

  const one = await event.from('sub_event').select('title, price, event(name, area(name))').single()
  if (one.data) {
    const event: string = one.data.event.name
    const price: number | null = one.data.price
    const area: string = one.data.event.area.name
    void [event, price, area]
  }
  const artist = await event.from('artist').select('name, genres').limit(1).single()
  const genres: unknown = artist.data?.genres
  void [title, genre, volume, chapter, genres]
}

export async function writes() {
  await novel.from('novel').insert({ title: 'T', author: 'A' })
  await novel.from('novel').insert([{ title: 'T', author: 'A', genre: null, status: 'ongoing' }])
  await novel.from('part').update({ word_count: 3 }).eq('id', 'x')
  await event.from('lineup').insert({ sub_event_id: 's', artist_id: 'a' })
  await todo.from('todo').insert({ list_id: 'l', title: 'T', user_id: 'u' })

  // @ts-expect-error: unknown column
  await novel.from('novel').insert({ title: 'T', author: 'A', nope: 1 })
  // @ts-expect-error: author is required
  await novel.from('novel').insert({ title: 'T' })
  // @ts-expect-error: word_count is a number
  await novel.from('part').update({ word_count: 'many' }).eq('id', 'x')
  // @ts-expect-error: unknown table
  await novel.from('nope').select()
}

export async function views() {
  const { data } = await novel.from('novel_stats').select('title, editions').eq('status', 'completed')
  if (!data) return
  const title: string | null = data[0].title
  void title
  // @ts-expect-error: a view has no Insert type
  await novel.from('novel_stats').insert({ title: 'T' })
}

export async function functions() {
  const { data } = await novel.rpc('search_novels', { q: 'Road' }).eq('status', 'completed').order('title')
  if (data) {
    const title: string | null = data[0].title
    void title
  }
  const count = await novel.rpc('novel_count', { status: 'hiatus' })
  const summary = await novel.rpc('novel_summary')
  const added = await novel.rpc('add_novel', { title: 'T', author: 'A' })
  const id: string | null | undefined = added.data?.id
  await todo.rpc('clear_done', { drop_empty: true })
  await todo.rpc('my_open_todos')
  void [count.data, summary.data, id]

  // @ts-expect-error: q is required
  await novel.rpc('search_novels', {})
  // @ts-expect-error: max is a number
  await novel.rpc('search_novels', { q: 'x', max: 'many' })
  // @ts-expect-error: unknown function
  await novel.rpc('nope')
}

export const row: Tables<'chapter'> = { id: 'c', volume_id: 'v', number: 1, title: 'T' }
export const statsRow: Tables<'novel_stats'> = { id: 'n', title: 'T', status: 's', editions: 1 }
export const insert: TablesInsert<'chapter'> = { volume_id: 'v', number: 1, title: 'T' }
