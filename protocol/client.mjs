// A minimal app-side client for tests: speaks protocol v1 exactly like the app's
// src/core/collab/network.ts, with the same yjs / y-protocols packages.
import { createHash } from 'node:crypto'
import * as decoding from 'lib0/decoding'
import * as encoding from 'lib0/encoding'
import * as awarenessProtocol from 'y-protocols/awareness'
import * as syncProtocol from 'y-protocols/sync'
import * as Y from 'yjs'

export const MSG = { hello: 0, welcome: 1, reject: 2, sync: 3, awareness: 4, assetRequest: 5, assetChunk: 6, closed: 7 }
export const SCHEMA = 1

const enc = new TextEncoder()
const dec = new TextDecoder()
const frame = (type, body) => {
  const out = new Uint8Array(body.length + 1)
  out[0] = type
  out.set(body, 1)
  return out
}
const jsonFrame = (type, value) => frame(type, enc.encode(JSON.stringify(value)))

export function deviceIdForKey(key) {
  return 'k-' + createHash('sha256').update(key).digest('hex').slice(0, 32)
}

export function decodeConnectCode(code) {
  const json = Buffer.from(code.slice('EGS1-'.length), 'base64url').toString()
  return JSON.parse(json)
}

export class TestClient {
  constructor(url, key, { name = 'Tester', schema = SCHEMA, doc = new Y.Doc() } = {}) {
    Object.assign(this, { url, key, name, schema, doc })
    this.awareness = new awarenessProtocol.Awareness(doc)
    this.assets = new Map() // path -> Uint8Array (files this "device" has)
    this.received = new Map() // path -> Uint8Array (fetched from the server)
    this.rejected = null
    this.closedMessage = null
    this.ready = false
    this.synced = false
    this.authError = null
    this.serverId = null
    this.peersAwareness = () => [...this.awareness.getStates().keys()].filter((id) => id !== doc.clientID)
  }

  connect() {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.url)
      this.ws = ws
      ws.binaryType = 'arraybuffer'
      let sentWelcome = false
      let gotWelcome = false
      const maybeReady = () => {
        if (this.ready || !sentWelcome || !gotWelcome) return
        this.ready = true
        const e = encoding.createEncoder()
        syncProtocol.writeSyncStep1(e, this.doc)
        this.send(frame(MSG.sync, encoding.toUint8Array(e)))
        this.send(frame(MSG.awareness, awarenessProtocol.encodeAwarenessUpdate(this.awareness, [this.doc.clientID])))
      }
      this.onDocUpdate = (update, origin) => {
        if (origin === 'remote' || !this.ready) return
        const e = encoding.createEncoder()
        syncProtocol.writeUpdate(e, update)
        this.send(frame(MSG.sync, encoding.toUint8Array(e)))
      }
      this.doc.on('update', this.onDocUpdate)
      this.awareness.setLocalStateField('user', { id: deviceIdForKey(this.key), name: this.name, color: '#123456' })
      ws.onopen = () => ws.send(JSON.stringify({ type: 'auth', key: this.key }))
      ws.onerror = (e) => reject(e)
      ws.onclose = () => {
        this.open = false
        this.doc.off('update', this.onDocUpdate)
        this.awareness.destroy() // stops its timer
        resolve(this)
      }
      ws.onmessage = (ev) => {
        if (typeof ev.data === 'string') {
          const m = JSON.parse(ev.data)
          if (m.type === 'auth-error') {
            this.authError = m.reason
            return resolve(this)
          }
          this.serverId = m.serverId
          this.role = m.role
          this.projectId = m.projectId
          this.open = true
          this.send(jsonFrame(MSG.hello, { protocol: 1, schema: this.schema, projectId: m.projectId, deviceId: deviceIdForKey(this.key), name: this.name, color: '#123456' }))
          return
        }
        const data = new Uint8Array(ev.data)
        const payload = data.subarray(1)
        switch (data[0]) {
          case MSG.hello: {
            const h = JSON.parse(dec.decode(payload))
            if (h.deviceId !== this.serverId) throw new Error('server id mismatch')
            this.send(jsonFrame(MSG.welcome, { projectId: h.projectId, projectName: '' }))
            sentWelcome = true
            maybeReady()
            break
          }
          case MSG.welcome:
            gotWelcome = true
            maybeReady()
            resolve(this)
            break
          case MSG.reject:
            this.rejected = JSON.parse(dec.decode(payload))
            resolve(this)
            break
          case MSG.closed:
            this.closedMessage = JSON.parse(dec.decode(payload))
            break
          case MSG.sync: {
            const d = decoding.createDecoder(payload)
            const e = encoding.createEncoder()
            const kind = syncProtocol.readSyncMessage(d, e, this.doc, 'remote')
            if (encoding.length(e) > 0) this.send(frame(MSG.sync, encoding.toUint8Array(e)))
            if (kind === syncProtocol.messageYjsSyncStep2) this.synced = true
            break
          }
          case MSG.awareness:
            awarenessProtocol.applyAwarenessUpdate(this.awareness, payload, 'remote')
            break
          case MSG.assetRequest: {
            const { path } = JSON.parse(dec.decode(payload))
            const bytes = this.assets.get(path)
            if (!bytes) this.send(chunkFrame({ path, offset: 0, total: 0, missing: true }, new Uint8Array(0)))
            else for (let o = 0; o < bytes.length; o += 512 * 1024) this.send(chunkFrame({ path, offset: o, total: bytes.length }, bytes.subarray(o, o + 512 * 1024)))
            break
          }
          case MSG.assetChunk: {
            const len = new DataView(payload.buffer, payload.byteOffset).getUint32(0)
            const header = JSON.parse(dec.decode(payload.subarray(4, 4 + len)))
            const bytes = payload.subarray(4 + len)
            if (header.missing) {
              this.received.set(header.path, null)
              break
            }
            let buf = this.incoming?.[header.path]
            if (!buf) (this.incoming ??= {})[header.path] = buf = { data: new Uint8Array(header.total), got: 0 }
            buf.data.set(bytes, header.offset)
            buf.got += bytes.length
            if (buf.got >= header.total) this.received.set(header.path, buf.data)
            break
          }
        }
      }
    })
  }

  send(data) {
    if (this.ws?.readyState === 1) this.ws.send(data)
  }

  requestAsset(path) {
    this.send(jsonFrame(MSG.assetRequest, { path }))
  }

  /** Raw update, bypassing the doc (to test that view keys can't change anything even if the app is modified). */
  sendRawUpdate(update) {
    const e = encoding.createEncoder()
    syncProtocol.writeUpdate(e, update)
    this.send(frame(MSG.sync, encoding.toUint8Array(e)))
  }

  close() {
    this.ws?.close()
    return new Promise((r) => setTimeout(r, 100))
  }
}

function chunkFrame(header, bytes) {
  const head = enc.encode(JSON.stringify(header))
  const out = new Uint8Array(5 + head.length + bytes.length)
  out[0] = MSG.assetChunk
  new DataView(out.buffer).setUint32(1, head.length)
  out.set(head, 5)
  out.set(bytes, 5 + head.length)
  return out
}

export const wait = (ms) => new Promise((r) => setTimeout(r, ms))

export async function until(check, what, ms = 5000) {
  const end = Date.now() + ms
  while (Date.now() < end) {
    if (check()) return
    await wait(50)
  }
  throw new Error(`Timed out waiting for: ${what}`)
}
