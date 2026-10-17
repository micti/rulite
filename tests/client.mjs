import { createHmac } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { createClient } from '@supabase/supabase-js'

// novel and event run without auth: any key works.
const options = { auth: { persistSession: false, autoRefreshToken: false } }

export const novel = createClient(process.env.RULITE_NOVEL_URL, 'dev-key', options)
export const event = createClient(process.env.RULITE_EVENT_URL, 'dev-key', options)

// todo runs with sample/todo.json: auth on.
export const todoUrl = process.env.RULITE_TODO_URL
export const anonKey = process.env.RULITE_ANON_KEY
export const serviceKey = process.env.RULITE_SERVICE_KEY
export const secret = JSON.parse(readFileSync(process.env.RULITE_TODO_CONFIG, 'utf8')).jwt_secret

/** A new client for the todo server; `headers` go on every request. */
export function todo(key = anonKey, headers = {}) {
  return createClient(todoUrl, key, { ...options, global: { headers } })
}

/** A new client signed in as `<name>@example.com`. */
export async function signedIn(name, password = 'secret1') {
  const client = todo()
  ok(await client.auth.signInWithPassword({ email: `${name}@example.com`, password }))
  return client
}

/** An HS256 JWT signed with `key` (the todo server's secret by default). */
export function sign(claims, key = secret, header = { alg: 'HS256', typ: 'JWT' }) {
  const b64 = (v) => Buffer.from(JSON.stringify(v)).toString('base64url')
  const body = `${b64(header)}.${b64(claims)}`
  return `${body}.${createHmac('sha256', key).update(body).digest('base64url')}`
}

/** The decoded header and claims of a JWT. */
export function decode(token) {
  const [header, claims] = token.split('.').slice(0, 2).map((p) => JSON.parse(Buffer.from(p, 'base64url')))
  return { header, claims }
}

/** `data` of a supabase-js result, failing with the PostgREST error if there is one. */
export function ok({ data, error }) {
  if (error) throw new Error(`${error.code}: ${error.message}`)
  return data
}

/** Unique-enough text for rows a test creates, so reruns and other tests don't collide. */
export function unique(prefix) {
  return `${prefix} ${Math.random().toString(16).slice(2, 10)}`
}
