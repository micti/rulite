// M17: CORS on the novel server (started with --cors-origins, see run.mjs). Raw fetch with an
// Origin header: Node does not enforce CORS, so the headers are checked by hand.

import { describe, test } from 'node:test'
import assert from 'node:assert/strict'

import { anonKey, novelUrl } from './client.mjs'

const allowed = process.env.RULITE_CORS_ORIGINS.split(',')[0]
const requestHeaders = 'apikey, authorization, content-type, x-client-info, prefer, range'

function preflight(path, origin = allowed, method = 'POST') {
  return fetch(`${novelUrl}${path}`, {
    method: 'OPTIONS',
    headers: { origin, 'access-control-request-method': method, 'access-control-request-headers': requestHeaders },
  })
}

describe('cors', () => {
  for (const path of ['/rest/v1/novel', '/auth/v1/token?grant_type=password']) {
    test(`preflight on ${path.split('?')[0]} is 204 with the CORS headers, before auth`, async () => {
      const res = await preflight(path)
      assert.equal(res.status, 204)
      const h = res.headers
      assert.equal(h.get('access-control-allow-origin'), allowed)
      assert.equal(h.get('vary'), 'Origin')
      assert.equal(h.get('access-control-allow-methods'), 'GET, HEAD, POST, PATCH, DELETE, OPTIONS')
      assert.equal(h.get('access-control-allow-headers'), requestHeaders)
      assert.equal(h.get('access-control-max-age'), '86400')
      assert.equal(h.get('access-control-allow-credentials'), null)
    })
  }

  test('a GET from an allowed origin echoes it and exposes Content-Range', async () => {
    const res = await fetch(`${novelUrl}/rest/v1/novel?select=title&limit=1`, {
      headers: { origin: allowed, apikey: anonKey, prefer: 'count=exact' },
    })
    assert.equal(res.status, 206)
    assert.equal(res.headers.get('access-control-allow-origin'), allowed)
    assert.equal(res.headers.get('vary'), 'Origin')
    assert.match(res.headers.get('access-control-expose-headers'), /\bContent-Range\b/)
    assert.match(res.headers.get('content-range'), /^0-0\/\d+$/)
  })

  test('errors carry the CORS headers too', async () => {
    for (const [path, status] of [['/rest/v1/novel', 401], ['/nope', 404]]) {
      const res = await fetch(`${novelUrl}${path}`, { headers: { origin: allowed } })
      assert.equal(res.status, status)
      assert.equal(res.headers.get('access-control-allow-origin'), allowed)
      assert.match(res.headers.get('access-control-expose-headers'), /Content-Range/)
    }
  })

  test('an unlisted origin gets no CORS headers', async () => {
    const res = await preflight('/rest/v1/novel', 'https://evil.example.com')
    assert.notEqual(res.status, 204)
    assert.equal(res.headers.get('access-control-allow-origin'), null)
    const get = await fetch(`${novelUrl}/rest/v1/novel?select=title`, {
      headers: { origin: 'https://evil.example.com', apikey: anonKey },
    })
    assert.equal(get.status, 200)
    assert.equal(get.headers.get('access-control-allow-origin'), null)
  })

  test('without an Origin header nothing changes', async () => {
    const res = await fetch(`${novelUrl}/rest/v1/novel?select=title`, { headers: { apikey: anonKey } })
    assert.equal(res.headers.get('access-control-allow-origin'), null)
    assert.equal(res.headers.get('vary'), null)
  })
})
