'use strict'
// Admin page. Plain JavaScript, no libraries, nothing loaded from outside.
// Text from people (names they typed in the app) is only ever set with textContent.

const $ = (sel) => document.querySelector(sel)
let csrf = null
let current = 'projects'
let refreshTimer = null

function h(tag, props, ...children) {
  const el = document.createElement(tag)
  for (const [k, v] of Object.entries(props ?? {})) {
    if (v === undefined || v === null || v === false) continue
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v)
    else if (k === 'class') el.className = v
    else if (k in el && typeof v !== 'string') el[k] = v
    else el.setAttribute(k, v === true ? '' : v)
  }
  for (const c of children.flat()) if (c !== null && c !== undefined && c !== false) el.append(c instanceof Node ? c : String(c))
  return el
}

async function api(method, path, body, raw = false) {
  const res = await fetch(`/admin/api/${path}`, {
    method,
    headers: { 'x-csrf': csrf ?? '', ...(raw ? {} : { 'content-type': 'application/json' }) },
    body: body === undefined ? undefined : raw ? body : JSON.stringify(body),
  })
  const data = (res.headers.get('content-type') ?? '').includes('json') ? await res.json() : null
  if (data?.csrf) csrf = data.csrf
  if (res.status === 401 && path !== 'login' && path !== 'password') return start()
  if (!res.ok) throw new Error(data?.error ?? `Something went wrong (${res.status}).`)
  return data
}

/** Runs an action from a button, showing errors next to it. */
function action(fn) {
  return async (event) => {
    const btn = event?.currentTarget
    const box = btn?.closest('section, .card, dialog, form')
    box?.querySelector(':scope > .error')?.remove()
    if (btn) btn.disabled = true
    try {
      await fn(event)
    } catch (e) {
      box?.append(h('p', { class: 'error' }, e.message))
    } finally {
      if (btn) btn.disabled = false
    }
  }
}

const bytes = (n) => (n < 1024 * 1024 ? `${Math.ceil(n / 1024)} KB` : n < 1024 ** 3 ? `${(n / 1024 ** 2).toFixed(1)} MB` : `${(n / 1024 ** 3).toFixed(2)} GB`)
const when = (t) => (t ? new Date(t * 1000).toLocaleString() : '')
const ago = (t) => {
  const m = Math.round((Date.now() / 1000 - t) / 60)
  return m < 1 ? 'just now' : m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`
}

function confirmDialog(title, message, confirmLabel, { danger = false, typeToConfirm } = {}) {
  return new Promise((resolve) => {
    const input = typeToConfirm ? h('input', { type: 'text', 'aria-label': `Type ${typeToConfirm} to confirm` }) : null
    const ok = h('button', { class: `btn ${danger ? 'danger' : 'primary'}`, onclick: () => close(true) }, confirmLabel)
    if (input) {
      ok.disabled = true
      input.addEventListener('input', () => (ok.disabled = input.value !== typeToConfirm))
    }
    const dlg = h(
      'dialog',
      null,
      h('h2', null, title),
      h('p', null, message),
      input && h('p', { class: 'small muted' }, `Type “${typeToConfirm}” to confirm.`),
      input,
      h('div', { class: 'row' }, ok, h('button', { class: 'btn', onclick: () => close(false) }, 'Cancel')),
    )
    function close(v) {
      dlg.close()
      dlg.remove()
      resolve(v)
    }
    dlg.addEventListener('cancel', () => close(false))
    document.body.append(dlg)
    dlg.showModal()
  })
}

function showDialog(...content) {
  const dlg = h('dialog', null, ...content, h('div', { class: 'row' }, h('button', { class: 'btn primary', onclick: () => (dlg.close(), dlg.remove()) }, 'Done')))
  document.body.append(dlg)
  dlg.showModal()
}

function copyButton(text) {
  return h('button', {
    class: 'btn',
    onclick: async (e) => {
      await navigator.clipboard.writeText(text)
      e.currentTarget.textContent = 'Copied'
    },
  }, 'Copy')
}

function page(...content) {
  const main = $('#main')
  main.replaceChildren(...content)
}

// ---- start ---------------------------------------------------------------------

async function start() {
  clearInterval(refreshTimer)
  const s = await api('GET', 'state')
  $('#version').textContent = s.version
  $('#server-name').textContent = s.serverName
  $('#tabs').hidden = !s.loggedIn
  $('#logout').hidden = !s.loggedIn
  if (!s.setup) return wizard()
  if (!s.loggedIn) return login()
  showTab(current)
}

function login() {
  const pw = h('input', { type: 'password', autocomplete: 'current-password', autofocus: true })
  const form = h('form', {
    class: 'card',
    onsubmit: (e) => {
      e.preventDefault()
      action(async () => {
        await api('POST', 'login', { password: pw.value })
        start()
      })({ currentTarget: form.querySelector('button') })
    },
  }, h('h2', null, 'Log in'), h('label', null, 'Admin password', pw), h('div', { class: 'row' }, h('button', { class: 'btn primary' }, 'Log in')))
  page(form)
  pw.focus()
}

const MODES = {
  'self-signed': ['On this computer or in my home network', 'The server makes its own certificate. Its fingerprint goes into every connect code, so the app knows it talks to exactly this server. Teammates must be able to reach this computer (same network, or a port forwarded in your router).'],
  proxy: ['Behind nginx or another web server', 'The server only listens on this computer (127.0.0.1). Your web server handles HTTPS and forwards connections. You get a ready-made nginx config.'],
  files: ['With a certificate I already have', 'For example from certbot or your company. Give the paths of the certificate and key files (PEM).'],
  acme: ['Public server with its own domain', 'Gets free certificates from Let’s Encrypt automatically. Needs a domain pointing to this server and port 443 open.'],
}

function modeChoices(selected, onchange) {
  return Object.entries(MODES).map(([value, [title, text]]) =>
    h('label', { class: 'choice' }, h('input', { type: 'radio', name: 'mode', value, checked: value === selected, onchange }), h('span', null, h('b', null, title), h('span', { class: 'small muted' }, text))),
  )
}

function wizard() {
  const name = h('input', { type: 'text', value: 'My Evelopment server', maxlength: 120 })
  const pw = h('input', { type: 'password', autocomplete: 'new-password' })
  const pw2 = h('input', { type: 'password', autocomplete: 'new-password' })
  const url = h('input', { type: 'text', placeholder: 'wss://games.example.com/sync' })
  const cert = h('input', { type: 'text', placeholder: '/etc/letsencrypt/live/example.com/fullchain.pem' })
  const key = h('input', { type: 'text', placeholder: '/etc/letsencrypt/live/example.com/privkey.pem' })
  const email = h('input', { type: 'email', placeholder: 'you@example.com (optional)' })
  const extra = h('div')
  const mode = () => document.querySelector('input[name=mode]:checked')?.value
  const update = () => {
    const m = mode()
    extra.replaceChildren(
      m !== 'self-signed' ? h('label', null, 'Address apps connect to', url) : h('p', { class: 'small muted' }, 'Apps connect to this computer’s address in your network. You can change it later in Settings.'),
      m === 'proxy' ? h('p', { class: 'small muted' }, 'The https address of your web server, ending in /sync, for example wss://games.example.com/sync.') : null,
      m === 'files' ? [h('label', null, 'Certificate file', cert), h('label', null, 'Key file', key)] : null,
      m === 'acme' ? [h('p', { class: 'small muted' }, 'Use wss://your-domain/sync as the address.'), h('label', null, 'Email for Let’s Encrypt (optional)', email)] : null,
    )
  }
  const form = h(
    'form',
    { class: 'card', onsubmit: (e) => e.preventDefault() },
    h('h2', null, 'Set up your server'),
    h('p', { class: 'muted' }, 'This takes a minute. Everything here can be changed later.'),
    h('h3', null, '1. Name'),
    h('label', null, 'Server name (people see it in the app)', name),
    h('h3', null, '2. Admin password'),
    h('p', { class: 'small muted' }, 'Protects this page. At least 12 characters; a few random words work well.'),
    h('label', null, 'Password', pw),
    h('label', null, 'Repeat password', pw2),
    h('h3', null, '3. Where people connect from'),
    modeChoices('self-signed', update),
    extra,
    h('h3', null, '4. Privacy'),
    h('p', { class: 'small muted' }, 'The server stores projects, the keys you make (only as hashes) and the name someone typed with each key. It never writes IP addresses to disk and sends nothing to the internet on its own. If you run it for other people, you are responsible for their data under the GDPR; templates are in the Privacy tab.'),
    h('div', { class: 'row' }, h('button', {
      class: 'btn primary',
      onclick: action(async () => {
        if (pw.value !== pw2.value) throw new Error('The two passwords are different.')
        await api('POST', 'setup', { serverName: name.value, password: pw.value, mode: mode(), publicUrl: url.value, certPath: cert.value, keyPath: key.value, acmeEmail: email.value })
        if (mode() !== 'proxy') $('#banner').hidden = false
        $('#banner').textContent = 'Setup saved. Restart the server once so it uses the connection mode you picked.'
        start()
      }),
    }, 'Finish setup')),
  )
  page(form)
  update()
}

// ---- tabs ------------------------------------------------------------------------

const TABS = { projects: 'Projects', connected: 'Connected now', plugins: 'Plugins', backups: 'Backups', settings: 'Settings', privacy: 'Privacy & legal', log: 'Admin log' }

function showTab(name) {
  current = name
  clearInterval(refreshTimer)
  $('#tabs').replaceChildren(...Object.entries(TABS).map(([k, label]) => h('button', { 'aria-current': k === name ? 'page' : null, onclick: () => showTab(k) }, label)))
  ;({ projects, connected, plugins, backups, settings, privacy, log })[name]().catch((e) => page(h('p', { class: 'error' }, e.message)))
}

async function overview() {
  const o = await api('GET', 'overview')
  const banner = $('#banner')
  if (o.restartNeeded) {
    banner.hidden = false
    banner.textContent = 'Restart the server so the new connection settings take effect.'
  }
  return o
}

async function projects() {
  const o = await overview()
  const name = h('input', { type: 'text', placeholder: 'Project name', maxlength: 120 })
  const create = h('section', null,
    h('h2', null, 'Projects'),
    h('p', { class: 'muted small' }, 'Make a project here, then a key for each person. They paste the connect code into Evelopment Games Designer (Join project). To move a project that is already shared, give its host a write key and use “Move to server” in the app.'),
    h('div', { class: 'row' }, name, h('button', { class: 'btn primary', onclick: action(async () => {
      await api('POST', 'projects', { name: name.value })
      projects()
    }) }, 'New project')),
  )
  const cards = o.projects.map(({ project: p, keys, assetBytes }) => projectCard(p, keys, assetBytes))
  page(create, ...(cards.length ? cards : [h('p', { class: 'muted' }, 'No projects yet.')]))
}

function projectCard(p, keyCount, assetBytes) {
  const keysBox = h('div')
  const restoreInput = h('input', { type: 'file', accept: '.zip', hidden: true, onchange: action(async () => {
    const file = restoreInput.files[0]
    if (!file) return
    if (!(await confirmDialog('Restore backup?', `Everyone working on “${p.name}” is disconnected, and the project goes back to the state in ${file.name}. Changes made since then are lost.`, 'Restore', { danger: true }))) return
    await api('POST', `projects/${p.id}/restore`, await file.arrayBuffer(), true)
    projects()
  }) })
  return h('div', { class: 'card' },
    h('h3', { style: null }, p.name, ' ', p.archived ? h('span', { class: 'tag' }, 'closed') : null),
    h('p', { class: 'small muted' }, `${keyCount} key${keyCount === 1 ? '' : 's'} · files ${bytes(assetBytes)} of ${p.quotaMb} MB · last used ${when(p.lastUsed)}`),
    h('div', { class: 'row' },
      h('button', { class: 'btn primary', onclick: action(() => showKeys(p, keysBox)) }, 'Keys'),
      h('button', { class: 'btn', onclick: action(async () => {
        const n = prompt('New name', p.name)
        if (n) await api('POST', `projects/${p.id}`, { name: n })
        projects()
      }) }, 'Rename'),
      h('button', { class: 'btn', onclick: action(async () => {
        const q = prompt('Storage for images and audio, in MB', p.quotaMb)
        if (q) await api('POST', `projects/${p.id}`, { quotaMb: Number(q) })
        projects()
      }) }, 'Storage'),
      h('a', { class: 'btn', href: `/admin/api/projects/${p.id}/backup`, download: true }, 'Download backup'),
      h('button', { class: 'btn', onclick: () => restoreInput.click() }, 'Restore…'),
      restoreInput,
      p.archived
        ? h('button', { class: 'btn', onclick: action(async () => (await api('POST', `projects/${p.id}`, { archived: false }), projects())) }, 'Reopen')
        : h('button', { class: 'btn', onclick: action(async () => {
            if (!(await confirmDialog('Close project?', `Everyone working on “${p.name}” is disconnected and can keep a copy on their computer. Nobody can connect until you reopen it. Nothing is deleted.`, 'Close project'))) return
            await api('POST', `projects/${p.id}`, { archived: true })
            projects()
          }) }, 'Close'),
      h('button', { class: 'btn danger', onclick: action(async () => {
        if (!(await confirmDialog('Delete project?', `“${p.name}”, its keys and its files are deleted from this server. People keep the copies on their own computers. This can’t be undone (except from a backup).`, 'Delete', { danger: true, typeToConfirm: p.name }))) return
        await api('DELETE', `projects/${p.id}`)
        projects()
      }) }, 'Delete'),
    ),
    keysBox,
  )
}

async function showKeys(p, box) {
  const keys = await api('GET', `projects/${p.id}/keys`)
  const label = h('input', { type: 'text', placeholder: 'Who is it for? e.g. Ana', maxlength: 120 })
  const role = h('select', null, h('option', { value: 'write' }, 'Can edit (write)'), h('option', { value: 'view' }, 'Can only look (view)'))
  const days = h('input', { type: 'number', min: 0, placeholder: 'never', class: 'narrow' })
  box.replaceChildren(
    h('h3', null, 'Keys'),
    h('p', { class: 'small muted' }, 'One key per person is best: you can then remove one person without bothering the others. Names are chosen by each person in the app and are not checked.'),
    keys.length
      ? h('table', null,
          h('tr', null, h('th', null, 'For'), h('th', null, 'Access'), h('th', null, 'Name they use'), h('th', null, 'Expires'), h('th')),
          keys.map((k) => h('tr', null,
            h('td', null, k.label),
            h('td', null, h('span', { class: `tag ${k.role}` }, k.role)),
            h('td', null, k.lastName || h('span', { class: 'muted' }, 'not used yet')),
            h('td', null, k.expires ? when(k.expires) : 'never'),
            h('td', null, h('button', { class: 'btn danger', onclick: action(async () => {
              if (!(await confirmDialog('Revoke key?', `“${k.label}” stops working right away. Whoever uses it is disconnected and can keep a copy on their computer.`, 'Revoke', { danger: true }))) return
              await api('DELETE', `keys/${k.hash}`)
              showKeys(p, box)
            }) }, 'Revoke')),
          )),
        )
      : h('p', { class: 'muted' }, 'No keys yet.'),
    h('div', { class: 'row' }, label, role, h('span', { class: 'small muted' }, 'expires after days'), days, h('button', { class: 'btn primary', onclick: action(async () => {
      const made = await api('POST', `projects/${p.id}/keys`, { label: label.value, role: role.value, expiresDays: days.value ? Number(days.value) : null })
      showDialog(
        h('h2', null, `Connect code for ${label.value}`),
        h('p', null, 'Send this to the person (for example in a direct message). In Evelopment Games Designer they choose Join project and paste it.'),
        h('div', { class: 'code' }, made.code),
        h('div', { class: 'row' }, copyButton(made.code)),
        h('p', { class: 'warn' }, 'This is shown only once. The server keeps only a fingerprint of the key, so it can’t show it again. Lost it? Revoke the key and make a new one.'),
      )
      showKeys(p, box)
    }) }, 'New key')),
  )
}

async function connected() {
  const o = await overview()
  page(h('section', null,
    h('h2', null, 'Connected now'),
    h('p', { class: 'small muted' }, 'Who is online right now. Names are what each person typed in the app. The server keeps no history of this and never stores IP addresses.'),
    o.connected.length
      ? h('table', null, h('tr', null, h('th', null, 'Name'), h('th', null, 'Project'), h('th', null, 'Access'), h('th', null, 'Connected for')),
          o.connected.map((c) => h('tr', null, h('td', null, c.name || '(no name)'), h('td', null, c.project), h('td', null, h('span', { class: `tag ${c.role}` }, c.role)), h('td', null, ago(c.since)))))
      : h('p', { class: 'muted' }, 'Nobody is connected.'),
  ))
  refreshTimer = setInterval(() => current === 'connected' && connected(), 5000)
}

async function plugins() {
  const o = await overview()
  const file = h('input', { type: 'file', accept: '.zip' })
  page(h('section', null,
    h('h2', null, 'Plugins'),
    h('p', null, 'Plugins installed here are offered to everyone who connects. Each person’s app asks before installing one, and again whenever you update it.'),
    h('div', { class: 'warn' }, h('b', null, 'Plugins are programs. '), 'A plugin runs inside each person’s app with the same rights as the app: it can read, change and delete any file on their computer. Nobody reviews plugins. Installing one here means you offer that program to everyone on this server, so only install plugins you trust completely.'),
    h('div', { class: 'row' }, file, h('button', { class: 'btn primary', onclick: action(async () => {
      if (!file.files[0]) throw new Error('Choose a plugin zip first.')
      await api('POST', 'plugins', await file.files[0].arrayBuffer(), true)
      plugins()
    }) }, 'Install')),
    h('p', { class: 'small muted' }, 'Use the same zip you would install in the app (plugin.json and index.js). Installing the same id again updates it.'),
  ),
  h('section', null,
    h('h3', null, 'Installed'),
    o.plugins.length
      ? h('table', null, h('tr', null, h('th', null, 'Plugin'), h('th', null, 'Version'), h('th', null, 'Fingerprint'), h('th')),
          o.plugins.map((p) => h('tr', null,
            h('td', null, h('b', null, p.name), h('div', { class: 'small muted' }, p.description)),
            h('td', null, p.version),
            h('td', { class: 'small' }, h('code', null, p.sha256.slice(0, 16) + '…')),
            h('td', null, h('button', { class: 'btn danger', onclick: action(async () => {
              if (!(await confirmDialog('Remove plugin?', `${p.name} is no longer offered. People who installed it keep it until they remove it in their app.`, 'Remove', { danger: true }))) return
              await api('DELETE', `plugins/${p.id}`)
              plugins()
            }) }, 'Remove')))))
      : h('p', { class: 'muted' }, 'No plugins installed.'),
  ))
}

async function backups() {
  const o = await overview()
  const s = o.settings
  const daily = h('input', { type: 'checkbox', checked: s.dailyBackups })
  const dir = h('input', { type: 'text', value: s.backupDir })
  page(h('section', null,
    h('h2', null, 'Backups'),
    h('p', null, 'A full backup copies the database, every project’s files and the plugins into a dated folder. Backups older than 14 days are deleted automatically. A single project can be downloaded and restored on the Projects tab.'),
    h('p', { class: 'small muted' }, 'Backups are not encrypted. Keep the folder on an encrypted disk (BitLocker, FileVault, LUKS) or copy it somewhere safe.'),
    h('label', { class: 'inline' }, daily, 'Make a full backup every day'),
    h('label', null, 'Backup folder', dir),
    h('div', { class: 'row' },
      h('button', { class: 'btn', onclick: action(async () => {
        await api('POST', 'settings', { dailyBackups: daily.checked, backupDir: dir.value })
        backups()
      }) }, 'Save'),
      h('button', { class: 'btn primary', onclick: action(async (e) => {
        const r = await api('POST', 'backup-now')
        e.currentTarget.closest('section').append(h('p', null, 'Saved to ', h('code', null, r.path)))
      }) }, 'Back up now'),
    ),
    h('p', { class: 'small muted' }, 'To restore a full backup: stop the server, copy the backup folder’s contents into the data folder ', h('code', null, s.dataDir), ', start the server.'),
  ))
}

function nginxConfig(s) {
  const port = s.syncAddr.split(':').pop()
  return `# Inside your server { ... } block for the domain, with HTTPS already set up.
# Only /sync is forwarded: the admin page stays on the server itself.
location /sync {
    proxy_pass http://127.0.0.1:${port};
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header X-Real-IP $remote_addr;
    proxy_read_timeout 120s;
    access_log off;   # no IP addresses in logs
}`
}

async function settings() {
  const o = await overview()
  const s = o.settings
  const name = h('input', { type: 'text', value: s.serverName })
  const url = h('input', { type: 'text', value: s.publicUrl, placeholder: s.syncUrl })
  const cert = h('input', { type: 'text', value: s.certPath })
  const key = h('input', { type: 'text', value: s.keyPath })
  const email = h('input', { type: 'email', value: s.acmeEmail })
  const trust = h('input', { type: 'checkbox', checked: s.trustProxy })
  const unused = h('input', { type: 'number', min: 0, value: s.deleteUnusedDays, class: 'narrow' })
  const modeBox = h('div', null, modeChoices(s.mode))
  const pwCurrent = h('input', { type: 'password', autocomplete: 'current-password' })
  const pwNext = h('input', { type: 'password', autocomplete: 'new-password' })
  page(
    h('section', null,
      h('h2', null, 'Settings'),
      h('label', null, 'Server name', name),
      h('h3', null, 'Connection'),
      modeBox,
      h('label', null, 'Address apps connect to', url),
      h('p', { class: 'small muted' }, 'Currently: ', h('code', null, s.syncUrl), ' · listening on ', h('code', null, s.syncAddr)),
      s.fingerprint ? h('p', { class: 'small muted' }, 'Certificate fingerprint (in every connect code): ', h('code', null, s.fingerprint)) : null,
      h('label', null, 'Certificate file (certificate mode)', cert),
      h('label', null, 'Key file (certificate mode)', key),
      h('label', null, 'Email for Let’s Encrypt (optional)', email),
      h('label', { class: 'inline' }, trust, 'Behind a reverse proxy: use its X-Real-IP header for the connection limits'),
      s.mode === 'proxy' ? [h('h3', null, 'nginx config'), h('textarea', { readonly: true, rows: 18 }, nginxConfig(s))] : null,
      h('h3', null, 'Keeping data'),
      h('label', null, 'Delete projects nobody opened for this many days (0 = never)', unused),
      h('div', { class: 'row' }, h('button', { class: 'btn primary', onclick: action(async () => {
        const r = await api('POST', 'settings', {
          serverName: name.value, mode: document.querySelector('input[name=mode]:checked').value, publicUrl: url.value,
          certPath: cert.value, keyPath: key.value, acmeEmail: email.value, trustProxy: trust.checked, deleteUnusedDays: Number(unused.value || 0),
        })
        $('#server-name').textContent = name.value
        settings()
        if (r.restartNeeded) $('#banner').hidden = false
      }) }, 'Save')),
    ),
    h('section', null,
      h('h3', null, 'Admin password'),
      h('label', null, 'Current password', pwCurrent),
      h('label', null, 'New password (at least 12 characters)', pwNext),
      h('div', { class: 'row' }, h('button', { class: 'btn', onclick: action(async (e) => {
        await api('POST', 'password', { current: pwCurrent.value, next: pwNext.value })
        e.currentTarget.closest('section').append(h('p', null, 'Password changed. Other admin sessions were logged out.'))
      }) }, 'Change password')),
    ),
  )
}

async function privacy() {
  const o = await overview()
  const s = o.settings
  const operator = h('input', { type: 'text', placeholder: 'Your name or company, address, email' })
  const lang = h('select', null, h('option', { value: 'de' }, 'Deutsch'), h('option', { value: 'en' }, 'English'))
  const text = h('textarea', { readonly: true })
  const fill = () => (text.value = privacyNotice(lang.value, operator.value || '[Name, Anschrift, E-Mail]', s))
  operator.addEventListener('input', fill)
  lang.addEventListener('change', fill)
  fill()
  page(h('section', null,
    h('h2', null, 'Privacy & legal'),
    h('p', null, 'Whoever runs this server is responsible for the personal data in it (the “controller” under the GDPR). Evelopment only provides the software.'),
    h('h3', null, 'What this server stores'),
    h('ul', null,
      h('li', null, 'Projects: everything people put into them, until you delete the project', s.deleteUnusedDays > 0 ? ` or nobody opened it for ${s.deleteUnusedDays} days.` : '.'),
      h('li', null, 'Keys: only a fingerprint (hash), the label you gave it, its access and expiry, and the last name someone typed when using it. Deleted with the key.'),
      h('li', null, 'Admin log: what was done on this page and when, for 90 days. No IP addresses.'),
      h('li', null, 'Backups: kept 14 days.'),
      h('li', null, 'Not stored: IP addresses (kept in memory only while connected, for connection limits), connection history, activity per person, cookies other than the login cookie of this page.'),
    ),
    h('p', { class: 'small muted' }, 'The server contacts nothing on the internet by itself' + (s.mode === 'acme' ? ', except Let’s Encrypt to get certificates.' : '.')),
    h('h3', null, 'Privacy notice for your users'),
    h('p', { class: 'small muted' }, 'A template filled in from your settings. Template, not legal advice: have it checked if you run the server for a company or the public. More templates (records of processing, TOMs, notes for companies) are in the docs/operators folder of the server’s download.'),
    h('div', { class: 'row' }, operator, lang),
    text,
    h('div', { class: 'row' }, h('button', { class: 'btn', onclick: () => navigator.clipboard.writeText(text.value) }, 'Copy')),
  ))
}

function privacyNotice(lang, operator, s) {
  const deleteNote = s.deleteUnusedDays > 0
  if (lang === 'de') {
    return `Datenschutzhinweise für den Evelopment-Server „${s.serverName}“
(Vorlage, keine Rechtsberatung)

1. Verantwortlicher
${operator}

2. Welche Daten verarbeitet werden und warum
- Projektinhalte, die Sie in Evelopment Games Designer anlegen (Texte, Bilder, Audio), um sie mit Ihrem Team zu synchronisieren.
- Ihr Zugangsschlüssel: Der Server speichert nur einen Hash des Schlüssels, die Bezeichnung, die Berechtigung (lesen/schreiben) und ein Ablaufdatum.
- Der Name, den Sie in der App eingeben (frei gewählt, nicht überprüft), damit Ihr Team sieht, wer gerade arbeitet. Gespeichert wird nur der zuletzt verwendete Name je Schlüssel.
- Ihre IP-Adresse wird nur während der Verbindung im Arbeitsspeicher gehalten, um Missbrauch zu begrenzen (Verbindungslimits). Sie wird nicht gespeichert oder protokolliert.
Rechtsgrundlage: Art. 6 Abs. 1 lit. b DSGVO (Bereitstellung der gemeinsamen Projektarbeit) bzw. lit. f (sicherer Betrieb). Im Beschäftigungsverhältnis ggf. § 26 BDSG.

3. Speicherdauer
- Projektinhalte: bis das Projekt gelöscht wird${deleteNote ? ` oder ${s.deleteUnusedDays} Tage lang nicht genutzt wurde` : ''}.
- Schlüssel und zugehöriger Name: bis der Schlüssel widerrufen wird oder abläuft.
- Sicherungskopien: 14 Tage.
- Es werden keine Verbindungs- oder Aktivitätsprotokolle gespeichert.

4. Cookies und Tracking
Es werden keine Cookies gesetzt und kein Tracking eingesetzt. Der Server lädt keine Inhalte von Dritten nach.

5. Empfänger
${s.mode === 'acme' ? 'Zertifikate werden bei Let’s Encrypt (ISRG, USA) bezogen; dabei werden keine Nutzerdaten übermittelt. ' : ''}Der Server läuft bei: [Hoster, Standort; Auftragsverarbeitungsvertrag nach Art. 28 DSGVO].

6. Ihre Rechte
Sie haben das Recht auf Auskunft (Art. 15), Berichtigung (Art. 16), Löschung (Art. 17), Einschränkung (Art. 18), Datenübertragbarkeit (Art. 20) und Widerspruch (Art. 21) sowie auf Beschwerde bei einer Datenschutzaufsichtsbehörde (Art. 77 DSGVO).

7. Sicherheit
Verbindungen sind mit TLS verschlüsselt. Projektinhalte sind auf dem Server nicht Ende-zu-Ende-verschlüsselt; der Betreiber kann sie technisch einsehen.
`
  }
  return `Privacy notice for the Evelopment server “${s.serverName}”
(template, not legal advice)

1. Controller
${operator}

2. What is processed and why
- Project content you create in Evelopment Games Designer (text, images, audio), to sync it with your team.
- Your access key: the server stores only a hash of it, its label, its access (view/write) and an expiry date.
- The name you type in the app (self-chosen, not verified), so your team sees who is working. Only the last name used with each key is stored.
- Your IP address is held in memory only while you are connected, to limit abuse (connection limits). It is never stored or logged.
Legal basis: Art. 6(1)(b) GDPR (providing the shared project work) and Art. 6(1)(f) (secure operation).

3. How long
- Project content: until the project is deleted${deleteNote ? ` or unused for ${s.deleteUnusedDays} days` : ''}.
- Keys and the name stored with them: until the key is revoked or expires.
- Backups: 14 days.
- No connection or activity logs are kept.

4. Cookies and tracking
No cookies, no tracking. The server loads nothing from third parties.

5. Recipients
${s.mode === 'acme' ? 'Certificates come from Let’s Encrypt (ISRG, USA); no user data is sent to them. ' : ''}The server is hosted by: [hoster, location; data processing agreement under Art. 28 GDPR].

6. Your rights
Access (Art. 15), rectification (Art. 16), erasure (Art. 17), restriction (Art. 18), portability (Art. 20), objection (Art. 21), and complaint to a supervisory authority (Art. 77 GDPR).

7. Security
Connections are encrypted with TLS. Project content is not end-to-end encrypted on the server; its operator can technically read it.
`
}

async function log() {
  const rows = await api('GET', 'audit')
  page(h('section', null,
    h('h2', null, 'Admin log'),
    h('p', { class: 'small muted' }, 'What was done on this page, kept 90 days. No IP addresses.'),
    rows.length ? h('table', null, rows.map((r) => h('tr', null, h('td', { class: 'small muted' }, when(r.at)), h('td', null, r.action)))) : h('p', { class: 'muted' }, 'Nothing yet.'),
  ))
}

$('#logout').addEventListener('click', async () => {
  await api('POST', 'logout')
  csrf = null
  start()
})

start().catch((e) => page(h('p', { class: 'error' }, e.message)))
