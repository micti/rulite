// T12.3: /auth/v1 through supabase-js on the todo server (sample/todo.json, reuse interval 2 s).
// alice and bob only sign in; carol's password and sessions are changed here.

import { describe, test } from 'node:test'
import assert from 'node:assert/strict'
import { setTimeout as sleep } from 'node:timers/promises'

import { anonKey, decode, ok, signedIn, todo, todoUrl } from './client.mjs'

const REFRESH_TOKEN = /^[a-z0-9]{12}$/

describe('sign in', () => {
  test('email + password returns a GoTrue session', async () => {
    const client = todo()
    const { session, user } = ok(await client.auth.signInWithPassword({ email: 'alice@example.com', password: 'secret1' }))
    assert.equal(session.token_type, 'bearer')
    assert.equal(session.expires_in, 3600)
    assert.match(session.refresh_token, REFRESH_TOKEN)
    assert.equal(user.email, 'alice@example.com')
    assert.equal(user.aud, 'authenticated')
    assert.equal(user.role, 'authenticated')
    assert.deepEqual(user.app_metadata, { provider: 'email', providers: ['email'] })
    assert.ok(user.email_confirmed_at)
    assert.ok(user.last_sign_in_at)
    assert.match(user.id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
  })

  test('email is case-insensitive', async () => {
    ok(await todo().auth.signInWithPassword({ email: 'Alice@Example.COM', password: 'secret1' }))
  })

  test('wrong password and unknown email fail the same way', async () => {
    for (const email of ['alice@example.com', 'nobody@example.com']) {
      const { data, error } = await todo().auth.signInWithPassword({ email, password: 'wrong-password' })
      assert.equal(error.code, 'invalid_credentials')
      assert.equal(error.status, 400)
      assert.equal(data.session, null)
    }
  })

  test('sign-up is disabled', async () => {
    const { error } = await todo().auth.signUp({ email: 'new@example.com', password: 'secret1' })
    assert.equal(error.code, 'signup_disabled')
    assert.equal(error.status, 422)
  })

  test('a valid apikey is required', async () => {
    const res = await fetch(`${todoUrl}/auth/v1/settings`)
    assert.equal(res.status, 401)
    assert.equal(res.headers.get('x-supabase-api-version'), '2024-01-01')
    const ok = await fetch(`${todoUrl}/auth/v1/settings`, { headers: { apikey: anonKey } })
    assert.equal((await ok.json()).disable_signup, true)
    const missing = await fetch(`${todoUrl}/auth/v1/nope`, { headers: { apikey: anonKey } })
    assert.equal(missing.status, 404)
  })
})

describe('tokens', () => {
  test('access token header and claims match GoTrue', async () => {
    const client = await signedIn('alice')
    const { data: { session } } = await client.auth.getSession()
    const { header, claims } = decode(session.access_token)
    assert.deepEqual(header, { alg: 'HS256', typ: 'JWT' })
    assert.deepEqual(Object.keys(claims), [
      'iss', 'sub', 'aud', 'exp', 'iat', 'email', 'phone', 'app_metadata', 'user_metadata',
      'role', 'aal', 'amr', 'session_id', 'is_anonymous',
    ])
    assert.equal(claims.iss, `${todoUrl}/auth/v1`)
    assert.equal(claims.sub, session.user.id)
    assert.equal(claims.aud, 'authenticated')
    assert.equal(claims.role, 'authenticated')
    assert.equal(claims.email, 'alice@example.com')
    assert.equal(claims.phone, '')
    assert.equal(claims.aal, 'aal1')
    assert.equal(claims.amr[0].method, 'password')
    assert.equal(typeof claims.amr[0].timestamp, 'number')
    assert.equal(claims.is_anonymous, false)
    assert.equal(session.expires_at, claims.iat + session.expires_in)
    assert.equal(claims.exp, session.expires_at)
  })

  test('API keys are legacy Supabase keys', () => {
    const { header, claims } = decode(anonKey)
    assert.deepEqual(header, { alg: 'HS256', typ: 'JWT' })
    assert.deepEqual(Object.keys(claims), ['iss', 'ref', 'role', 'iat', 'exp'])
    assert.equal(claims.iss, 'supabase')
    assert.equal(claims.role, 'anon')
  })
})

describe('user', () => {
  test('getUser returns the signed-in user', async () => {
    const client = await signedIn('bob')
    const { user } = ok(await client.auth.getUser())
    assert.equal(user.email, 'bob@example.com')
    assert.deepEqual(user.identities, [])
  })

  test('updateUser merges user_metadata', async () => {
    const client = await signedIn('bob')
    ok(await client.auth.updateUser({ data: { name: 'Bob', theme: 'dark' } }))
    const { user } = ok(await client.auth.updateUser({ data: { theme: null } }))
    assert.deepEqual(user.user_metadata, { name: 'Bob' })
  })

  test('change password, then sign in with it', async () => {
    const client = await signedIn('carol')
    const other = await signedIn('carol')
    ok(await client.auth.updateUser({ password: 'secret2' }))
    const { error } = await todo().auth.signInWithPassword({ email: 'carol@example.com', password: 'secret1' })
    assert.equal(error.code, 'invalid_credentials')
    const again = await signedIn('carol', 'secret2')
    // The changing session stays, the other one is signed out.
    ok(await client.auth.getUser())
    assert.ok((await other.auth.getUser()).error)
    ok(await again.auth.updateUser({ password: 'secret1' }))
  })

  test('a short password is a weak_password', async () => {
    const client = await signedIn('carol')
    const { error } = await client.auth.updateUser({ password: '123' })
    assert.equal(error.code, 'weak_password')
    assert.equal(error.status, 422)
    assert.deepEqual(error.reasons, ['length'])
  })
})

describe('refresh', () => {
  test('rotates the refresh token', async () => {
    const client = await signedIn('alice')
    const { data: { session: before } } = await client.auth.getSession()
    const { session } = ok(await client.auth.refreshSession())
    assert.match(session.refresh_token, REFRESH_TOKEN)
    assert.notEqual(session.refresh_token, before.refresh_token)
    assert.equal(decode(session.access_token).claims.session_id, decode(before.access_token).claims.session_id)
    ok(await client.from('list').select('id'))
  })

  test('an old token within the reuse interval returns the current one', async () => {
    const client = await signedIn('alice')
    const { data: { session: first } } = await client.auth.getSession()
    const { session: second } = ok(await client.auth.refreshSession())
    const { session: reused } = ok(await todo().auth.refreshSession({ refresh_token: first.refresh_token }))
    assert.equal(reused.refresh_token, second.refresh_token)
  })

  test('an old token after the reuse interval revokes the session', async () => {
    const client = await signedIn('alice')
    const { data: { session: first } } = await client.auth.getSession()
    const { session: second } = ok(await client.auth.refreshSession())
    await sleep(2500)
    let { error } = await todo().auth.refreshSession({ refresh_token: first.refresh_token })
    assert.equal(error.code, 'refresh_token_already_used')
    ;({ error } = await todo().auth.refreshSession({ refresh_token: second.refresh_token }))
    assert.equal(error.code, 'refresh_token_not_found')
  })

  test('an unknown token', async () => {
    const { error } = await todo().auth.refreshSession({ refresh_token: 'abcdefghijkl' })
    assert.equal(error.code, 'refresh_token_not_found')
    assert.equal(error.status, 400)
  })
})

describe('sign out', () => {
  test('then refresh fails', async () => {
    const client = await signedIn('alice')
    const { data: { session } } = await client.auth.getSession()
    ok(await client.auth.signOut({ scope: 'local' }))
    const { error } = await todo().auth.refreshSession({ refresh_token: session.refresh_token })
    assert.equal(error.code, 'refresh_token_not_found')
    // The access token stays valid until it expires, but its session is gone.
    const stale = todo(anonKey, { Authorization: `Bearer ${session.access_token}` })
    assert.ok((await stale.auth.getUser(session.access_token)).error)
    ok(await stale.from('list').select('id'))
  })

  test('scope others keeps the current session', async () => {
    const a = await signedIn('carol')
    const b = await signedIn('carol')
    ok(await a.auth.signOut({ scope: 'others' }))
    ok(await a.auth.getUser())
    const { data: { session } } = await b.auth.getSession()
    assert.ok((await todo().auth.refreshSession({ refresh_token: session.refresh_token })).error)
  })
})
