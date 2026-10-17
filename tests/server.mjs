// Starts rulite servers for run.mjs and for tests that need a server of their own.

import { spawn } from 'node:child_process'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
export const bin = process.env.RULITE_BIN ?? join(root, 'target/release/rulite')

/** The environment without the caller's RULITE_* settings, plus `extra`. */
export function serverEnv(extra = {}) {
  const env = { ...process.env }
  for (const name of Object.keys(env)) if (name.startsWith('RULITE_')) delete env[name]
  return { ...env, ...extra }
}

/**
 * Starts `bin args` on port 0 and resolves `{ url, child }` once rulite prints its
 * "serving ... on <url>" line; rejects with the log if it exits first.
 */
export function start(args, { env = serverEnv(), program = bin } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(program, [...args, '--bind', '127.0.0.1:0'], { stdio: ['ignore', 'inherit', 'pipe'], env })
    let log = ''
    child.stderr.setEncoding('utf8')
    child.stderr.on('data', (chunk) => {
      log += chunk
      const m = log.match(/ on (https?:\/\/\S+)\n/)
      if (m) resolve({ url: m[1], child, log: () => log })
    })
    child.on('exit', (code) => reject(new Error(`rulite exited with ${code}:\n${log}`)))
    child.on('error', reject)
  })
}

/** Runs `bin args` to the end: `{ status, stderr }`. */
export function exited(args, { env = serverEnv(), program = bin } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(program, args, { stdio: ['ignore', 'ignore', 'pipe'], env })
    let stderr = ''
    child.stderr.setEncoding('utf8')
    child.stderr.on('data', (chunk) => (stderr += chunk))
    child.on('exit', (status) => resolve({ status, stderr }))
    child.on('error', reject)
  })
}
