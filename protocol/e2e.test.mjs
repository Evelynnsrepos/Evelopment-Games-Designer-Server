// End-to-end test: the real server binary and app-like clients using the app's yjs packages.
// Run: cargo build (in server/), then `npm test` here.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { after, before, test } from 'node:test'
import * as Y from 'yjs'
import { decodeConnectCode, TestClient, until, wait } from './client.mjs'

const exe = join(import.meta.dirname, '..', 'server', 'target', 'debug', process.platform === 'win32' ? 'egd-server.exe' : 'egd-server')
const data = mkdtempSync(join(tmpdir(), 'egd-server-test-'))
const SYNC = 18474 + Math.floor(Math.random() * 500)
const ADMIN = SYNC + 1000
const sync = `ws://127.0.0.1:${SYNC}/sync`
let server
let cookie = ''
let csrf = ''

async function start() {
  server = spawn(exe, ['--data', data, '--no-browser', '--headless'], {
    env: { ...process.env, EGD_SYNC_ADDR: `127.0.0.1:${SYNC}`, EGD_ADMIN_ADDR: `127.0.0.1:${ADMIN}` },
    stdio: ['ignore', 'pipe', 'inherit'],
  })
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${SYNC}/health`)).ok) return
    } catch {}
    await wait(100)
  }
  throw new Error('server did not start')
}

async function stop() {
  if (!server) return
  const done = new Promise((r) => server.once('exit', r))
  server.kill()
  await done
  server = null
}

async function api(method, path, body, raw = false) {
  const res = await fetch(`http://127.0.0.1:${ADMIN}/admin/api/${path}`, {
    method,
    headers: { cookie, 'x-csrf': csrf, ...(raw ? {} : { 'content-type': 'application/json' }) },
    body: body === undefined ? undefined : raw ? body : JSON.stringify(body),
  })
  const set = res.headers.get('set-cookie')
  if (set) cookie = set.split(';')[0]
  const type = res.headers.get('content-type') ?? ''
  const out = type.includes('json') ? await res.json() : new Uint8Array(await res.arrayBuffer())
  if (out?.csrf) csrf = out.csrf
  if (!res.ok) throw Object.assign(new Error(out?.error ?? res.status), { status: res.status })
  return out
}

let projectId
let writeKey
let writeKey2
let viewKey
const ID = '0f8fad5b-d9cb-469f-a165-70867728950e'
const ASSET = `assets/images/${ID}.png`

before(async () => {
  await start()
  const state = await api('GET', 'state')
  assert.equal(state.setup, false)
  await assert.rejects(api('POST', 'setup', { serverName: 'Test', password: 'short', mode: 'proxy' }))
  await api('POST', 'setup', { serverName: 'Test server', password: 'blue horse staple 42', mode: 'proxy' })
  projectId = (await api('POST', 'projects', { name: 'Space Game' })).id
  const k1 = await api('POST', `projects/${projectId}/keys`, { label: 'Ana', role: 'write' })
  const k2 = await api('POST', `projects/${projectId}/keys`, { label: 'Ben', role: 'write' })
  const k3 = await api('POST', `projects/${projectId}/keys`, { label: 'Guest', role: 'view' })
  ;[writeKey, writeKey2, viewKey] = [k1.key, k2.key, k3.key]
  const code = decodeConnectCode(k1.code)
  assert.equal(code.k, writeKey)
  assert.equal(code.p, projectId)
  assert.equal(code.u, sync)
})

after(async () => {
  await stop()
  rmSync(data, { recursive: true, force: true })
})

test('admin API needs a login and the CSRF token', async () => {
  const saved = [cookie, csrf]
  csrf = 'wrong'
  await assert.rejects(api('POST', 'projects', { name: 'x' }), (e) => e.status === 403)
  cookie = ''
  await assert.rejects(api('GET', 'overview'), (e) => e.status === 401)
  ;[cookie, csrf] = saved
  const o = await api('GET', 'overview')
  assert.equal(o.projects.length, 1)
})

test('a wrong key is refused', async () => {
  const c = await new TestClient(sync, 'egd-key-nope').connect()
  assert.equal(c.authError, 'bad-key')
})

test('two people sync through the server without ever being online together', async () => {
  const ana = await new TestClient(sync, writeKey, { name: 'Ana' }).connect()
  assert.equal(ana.role, 'write')
  await until(() => ana.synced, 'ana synced')
  ana.doc.getMap('meta').set('name', 'Space Game')
  ana.doc.getMap('doc:story/1').set('title', 'Chapter one')
  ana.assets.set(ASSET, new Uint8Array(700 * 1024).fill(7)) // two chunks
  ana.doc.getMap('doc:board/1').set('image', ASSET)
  // The server asks Ana for the image it now sees mentioned.
  await wait(4500)
  await ana.close()

  const ben = await new TestClient(sync, writeKey2, { name: 'Ben' }).connect()
  await until(() => ben.doc.getMap('doc:story/1').get('title') === 'Chapter one', 'ben gets ana’s edit')
  ben.requestAsset(ASSET)
  await until(() => ben.received.has(ASSET), 'ben gets the image from the server')
  assert.equal(ben.received.get(ASSET).length, 700 * 1024)
  assert.equal(ben.received.get(ASSET)[1000], 7)
  await ben.close()
})

test('edits and presence are relayed live', async () => {
  const ana = await new TestClient(sync, writeKey, { name: 'Ana' }).connect()
  const ben = await new TestClient(sync, writeKey2, { name: 'Ben' }).connect()
  await until(() => ana.synced && ben.synced, 'both synced')
  ana.doc.getMap('meta').set('genre', 'sci-fi')
  await until(() => ben.doc.getMap('meta').get('genre') === 'sci-fi', 'live edit')
  await until(() => ben.peersAwareness().length === 1, 'ben sees ana’s presence')
  const o = await api('GET', 'overview')
  assert.deepEqual(o.connected.map((c) => c.name).sort(), ['Ana', 'Ben'])
  assert.ok(o.connected.every((c) => !('ip' in c)))
  await ana.close()
  await until(() => ben.peersAwareness().length === 0, 'ana’s presence removed when she leaves')
  await ben.close()
})

test('a view key can read but never change anything', async () => {
  const guest = await new TestClient(sync, viewKey, { name: 'Guest' }).connect()
  assert.equal(guest.role, 'view')
  await until(() => guest.doc.getMap('doc:story/1').get('title') === 'Chapter one', 'guest reads')
  // Even a modified app sending changes directly is ignored...
  const other = new Y.Doc()
  other.getMap('doc:story/1').set('title', 'Vandalized')
  guest.sendRawUpdate(Y.encodeStateAsUpdate(other))
  await wait(300)
  const ben = await new TestClient(sync, writeKey2, { name: 'Ben' }).connect()
  await until(() => ben.synced, 'ben synced')
  assert.equal(ben.doc.getMap('doc:story/1').get('title'), 'Chapter one')
  // ...and after a few tries the connection is dropped.
  guest.sendRawUpdate(Y.encodeStateAsUpdate(other))
  guest.sendRawUpdate(Y.encodeStateAsUpdate(other))
  await until(() => !guest.open, 'guest disconnected')
  await ben.close()
})

test('an older or newer app is told to update', async () => {
  const c = await new TestClient(sync, writeKey, { schema: 99 }).connect()
  assert.equal(c.rejected?.reason, 'version')
  assert.equal(c.rejected?.schema, 1)
  await c.close()
})

test('revoking a key disconnects it at once and it stops working', async () => {
  const k = await api('POST', `projects/${projectId}/keys`, { label: 'Temp', role: 'write' })
  const c = await new TestClient(sync, k.key, { name: 'Temp' }).connect()
  await until(() => c.synced, 'temp synced')
  const keys = await api('GET', `projects/${projectId}/keys`)
  const temp = keys.find((x) => x.label === 'Temp')
  assert.equal(temp.lastName, 'Temp')
  assert.ok(!('key' in temp))
  await api('DELETE', `keys/${temp.hash}`)
  await until(() => c.rejected?.reason === 'not-member' && !c.open, 'kicked')
  const again = await new TestClient(sync, k.key).connect()
  assert.equal(again.authError, 'bad-key')
})

test('plugins: installed on the server, listed and downloaded with a key', async () => {
  const zip = await makePluginZip()
  const info = await api('POST', 'plugins', zip, true)
  assert.equal(info.id, 'dice-roller')
  const unauth = await fetch(`http://127.0.0.1:${SYNC}/plugins`)
  assert.equal(unauth.status, 401)
  const list = await (await fetch(`http://127.0.0.1:${SYNC}/plugins`, { headers: { authorization: `Bearer ${viewKey}` } })).json()
  assert.equal(list[0].id, 'dice-roller')
  const dl = await fetch(`http://127.0.0.1:${SYNC}/plugins/dice-roller.zip`, { headers: { authorization: `Bearer ${viewKey}` } })
  assert.equal(Buffer.from(await dl.arrayBuffer()).length, zip.length)
})

test('backup, close, restore, and everything survives a restart', async () => {
  const backup = await api('GET', `projects/${projectId}/backup`)
  assert.ok(backup.length > 100)
  const ben = await new TestClient(sync, writeKey2, { name: 'Ben' }).connect()
  await until(() => ben.synced, 'ben synced')
  ben.doc.getMap('doc:story/1').set('title', 'Changed later')
  await wait(200)
  // Restoring disconnects everyone with the "host closed" goodbye, then puts the old state back.
  await api('POST', `projects/${projectId}/restore`, backup, true)
  await until(() => ben.closedMessage?.reason === 'host-closed', 'ben told')
  await stop()
  await start()
  const ana = await new TestClient(sync, writeKey, { name: 'Ana' }).connect()
  await until(() => ana.doc.getMap('doc:story/1').get('title') === 'Chapter one', 'restored state after restart')
  ana.requestAsset(ASSET)
  await until(() => ana.received.get(ASSET)?.length === 700 * 1024, 'asset restored')
  await ana.close()
})

test('closing a project ends it for everyone and refuses new connections', async () => {
  const ana = await new TestClient(sync, writeKey, { name: 'Ana' }).connect()
  await until(() => ana.synced, 'synced')
  // The admin session was lost with the restart: log in again.
  await api('POST', 'login', { password: 'blue horse staple 42' })
  await api('POST', `projects/${projectId}`, { archived: true })
  await until(() => ana.closedMessage?.reason === 'host-closed', 'host closed')
  const again = await new TestClient(sync, writeKey).connect()
  assert.equal(again.authError, 'closed')
  const audit = await api('GET', 'audit')
  assert.ok(audit.some((a) => a.action.startsWith('Closed project')))
})

async function makePluginZip() {
  // A stored (uncompressed) zip built by hand, so the test needs no zip library.
  const files = [
    ['plugin.json', JSON.stringify({ id: 'dice-roller', name: 'Dice Roller', version: '1.0.0', apiVersion: 1 })],
    ['index.js', 'export default function plugin(egd) { return { View: () => null } }'],
  ]
  const { crc32 } = await import('node:zlib')
  const parts = []
  const central = []
  let offset = 0
  for (const [name, text] of files) {
    const n = Buffer.from(name)
    const body = Buffer.from(text)
    const crc = crc32(body)
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt32LE(crc, 14)
    local.writeUInt32LE(body.length, 18)
    local.writeUInt32LE(body.length, 22)
    local.writeUInt16LE(n.length, 26)
    parts.push(local, n, body)
    const c = Buffer.alloc(46)
    c.writeUInt32LE(0x02014b50, 0)
    c.writeUInt16LE(20, 4)
    c.writeUInt16LE(20, 6)
    c.writeUInt32LE(crc, 16)
    c.writeUInt32LE(body.length, 20)
    c.writeUInt32LE(body.length, 24)
    c.writeUInt16LE(n.length, 28)
    c.writeUInt32LE(offset, 42)
    central.push(c, n)
    offset += 30 + n.length + body.length
  }
  const cd = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(files.length, 8)
  end.writeUInt16LE(files.length, 10)
  end.writeUInt32LE(cd.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...parts, cd, end])
}
