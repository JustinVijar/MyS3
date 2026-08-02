const authGate = document.getElementById('auth-gate');
const appShell = document.getElementById('app-shell');
const authBootstrap = document.getElementById('auth-bootstrap');
const authLogin = document.getElementById('auth-login');
const authStatus = document.getElementById('auth-status');
const credReveal = document.getElementById('cred-reveal');
const listingBody = document.getElementById('listing-body');
const emptyState = document.getElementById('empty-state');
const breadcrumbsEl = document.getElementById('breadcrumbs');
const searchInput = document.getElementById('search');
const fileInput = document.getElementById('file-input');
const folderInput = document.getElementById('folder-input');
const uploadBtn = document.getElementById('upload-btn');
const uploadMenu = document.getElementById('upload-menu');
const newFolderBtn = document.getElementById('new-folder-btn');
const statusEl = document.getElementById('status');
const bucketChip = document.getElementById('bucket-chip');
const viewExplore = document.getElementById('view-explore');
const viewSettings = document.getElementById('view-settings');
const tabExplore = document.getElementById('tab-explore');
const tabSettings = document.getElementById('tab-settings');

const SESSION_KEY = 'mys3_session_token';
const BUCKET_KEY = 'mys3_bucket';

/** @type {{ authenticated: boolean, needs_bootstrap: boolean, is_owner: boolean, account: object|null }} */
let authState = { authenticated: false, needs_bootstrap: false, is_owner: false, account: null };
/** @type {string} */
let currentPrefix = '';
/** @type {string} */
let searchQuery = '';
let searchTimer = null;
/** @type {string} */
let currentBucket = localStorage.getItem(BUCKET_KEY) || 'storage';
/** @type {'all'|'folders'|'images'|'video'|'audio'|'text'|'other'} */
let typeFilter = 'all';

function getToken() {
  return localStorage.getItem(SESSION_KEY) || '';
}

function setToken(token) {
  if (token) localStorage.setItem(SESSION_KEY, token);
  else localStorage.removeItem(SESSION_KEY);
}

function setBucket(name) {
  currentBucket = name || 'storage';
  localStorage.setItem(BUCKET_KEY, currentBucket);
  if (bucketChip) bucketChip.textContent = currentBucket;
}

function authHeaders(extra) {
  const h = Object.assign({}, extra || {});
  const t = getToken();
  if (t) h['Authorization'] = 'Bearer ' + t;
  return h;
}

async function api(path, opts) {
  const options = Object.assign({ headers: {} }, opts || {});
  options.headers = authHeaders(options.headers);
  const res = await fetch(path, options);
  return res;
}

function showAuthStatus(msg, isError) {
  authStatus.hidden = false;
  authStatus.textContent = msg;
  authStatus.classList.toggle('error', !!isError);
}

function showCreds(username, password) {
  authBootstrap.hidden = true;
  authLogin.hidden = true;
  credReveal.hidden = false;
  document.getElementById('cred-user').textContent = username;
  document.getElementById('cred-pass').textContent = password;
}

function parseRoute() {
  const raw = location.hash.replace(/^#\/?/, '');
  const parts = raw.split('/').filter(Boolean);
  if (parts[0] === 'settings') {
    return { view: 'settings', panel: parts[1] || 'accounts', prefix: '' };
  }
  // explore or legacy `#/` / `#/path`
  let prefixParts = parts;
  if (parts[0] === 'explore') prefixParts = parts.slice(1);
  let prefix = prefixParts.map(decodeURIComponent).join('/');
  if (prefix && !prefix.endsWith('/')) prefix += '/';
  return { view: 'explore', panel: null, prefix };
}

function applyRoute() {
  const route = parseRoute();
  const onSettings = route.view === 'settings';
  viewExplore.hidden = onSettings;
  viewSettings.hidden = !onSettings;
  tabExplore.classList.toggle('active', !onSettings);
  tabSettings.classList.toggle('active', onSettings);

  if (onSettings) {
    if (window.MyS3Settings) window.MyS3Settings.showPanel(route.panel);
    return;
  }

  currentPrefix = route.prefix;
  if (authState.authenticated) refresh();
}

function setHashExplore(prefix) {
  const path = prefix.replace(/\/$/, '');
  const next = path ? `#/explore/${path}` : '#/explore/';
  if (location.hash !== next) location.hash = next;
  else applyRoute();
}

function navigateToPrefix(prefix) {
  searchQuery = '';
  searchInput.value = '';
  clearExplorerSelection();
  setHashExplore(prefix);
}

const IMAGE_EXT = new Set([
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'ico', 'avif', 'heic', 'tif', 'tiff',
]);
const VIDEO_EXT = new Set(['mp4', 'webm', 'mov', 'mkv', 'avi', 'm4v']);
const AUDIO_EXT = new Set(['mp3', 'wav', 'ogg', 'flac', 'm4a', 'aac', 'opus']);
const TEXT_EXT = new Set([
  'txt', 'md', 'markdown', 'json', 'js', 'ts', 'tsx', 'jsx', 'css', 'html', 'htm', 'xml',
  'yml', 'yaml', 'toml', 'csv', 'log', 'rs', 'go', 'py', 'java', 'c', 'h', 'cpp', 'hpp',
  'sh', 'bash', 'zsh', 'sql', 'ini', 'cfg', 'conf', 'env',
]);

function fileExt(name) {
  const base = String(name).includes('/')
    ? String(name).slice(String(name).lastIndexOf('/') + 1)
    : String(name);
  const i = base.lastIndexOf('.');
  if (i <= 0) return '';
  return base.slice(i + 1).toLowerCase();
}

function classifyFileType(key) {
  const ext = fileExt(key);
  if (IMAGE_EXT.has(ext)) return 'images';
  if (VIDEO_EXT.has(ext)) return 'video';
  if (AUDIO_EXT.has(ext)) return 'audio';
  if (TEXT_EXT.has(ext)) return 'text';
  return 'other';
}

function rowMatchesFilter(kind, fileType) {
  if (typeFilter === 'all') return true;
  if (typeFilter === 'folders') return kind === 'folder';
  return kind === 'file' && fileType === typeFilter;
}

function applyTypeFilter() {
  const rows = listingBody.querySelectorAll('tr[data-kind]');
  let visible = 0;
  rows.forEach((tr) => {
    const kind = tr.dataset.kind;
    const fileType = tr.dataset.type || '';
    const show = rowMatchesFilter(kind, fileType);
    tr.hidden = !show;
    if (show) visible += 1;
  });
  const hasAnyRows = rows.length > 0;
  if (!hasAnyRows) {
    emptyState.hidden = false;
    emptyState.textContent = searchQuery
      ? 'No matching objects.'
      : 'No objects here. Upload a file to get started.';
  } else if (visible === 0) {
    emptyState.hidden = false;
    emptyState.textContent = 'No items match this filter.';
  } else {
    emptyState.hidden = true;
  }
  updateExplorerSelectionUi();
}

function clearExplorerSelection() {
  const selectAll = document.getElementById('listing-select-all');
  if (selectAll) {
    selectAll.checked = false;
    selectAll.indeterminate = false;
  }
  listingBody.querySelectorAll('input[data-explorer-key]').forEach((el) => {
    el.checked = false;
  });
  updateExplorerSelectionUi();
}

function visibleExplorerChecks() {
  return Array.from(listingBody.querySelectorAll('tr[data-kind]:not([hidden]) input[data-explorer-key]'));
}

function updateExplorerSelectionUi() {
  const boxes = visibleExplorerChecks();
  const checked = boxes.filter((b) => b.checked);
  const selectAll = document.getElementById('listing-select-all');
  const bulk = document.getElementById('explorer-bulk-actions');
  const countEl = document.getElementById('explorer-selection-count');
  const dlBtn = document.getElementById('explorer-download-selected');
  const delBtn = document.getElementById('explorer-delete-selected');
  if (selectAll) {
    selectAll.disabled = boxes.length === 0;
    selectAll.checked = boxes.length > 0 && checked.length === boxes.length;
    selectAll.indeterminate = checked.length > 0 && checked.length < boxes.length;
  }
  if (bulk) bulk.hidden = checked.length === 0;
  if (countEl) {
    countEl.textContent = checked.length
      ? `${checked.length} selected`
      : '';
  }
  if (dlBtn) {
    const fileSelected = checked.some((b) => b.dataset.kind === 'file');
    dlBtn.disabled = !fileSelected;
  }
  if (delBtn) delBtn.disabled = checked.length === 0;
}

function selectedExplorerItems() {
  return visibleExplorerChecks()
    .filter((b) => b.checked)
    .map((b) => ({
      key: b.dataset.explorerKey,
      kind: b.dataset.kind,
      size: Number(b.dataset.size || 0),
    }));
}

function renderBreadcrumbs() {
  breadcrumbsEl.innerHTML = '';
  const parts = currentPrefix.split('/').filter(Boolean);

  const root = document.createElement(parts.length || searchQuery ? 'a' : 'span');
  if (parts.length || searchQuery) {
    root.href = '#/explore/';
    root.textContent = currentBucket;
    root.addEventListener('click', (e) => {
      e.preventDefault();
      navigateToPrefix('');
    });
  } else {
    root.className = 'current';
    root.textContent = currentBucket;
  }
  breadcrumbsEl.appendChild(root);

  let acc = '';
  parts.forEach((part, i) => {
    acc += part + '/';
    const sep = document.createElement('span');
    sep.className = 'sep';
    sep.textContent = '/';
    breadcrumbsEl.appendChild(sep);

    const isLast = i === parts.length - 1 && !searchQuery;
    if (isLast) {
      const cur = document.createElement('span');
      cur.className = 'current';
      cur.textContent = part;
      breadcrumbsEl.appendChild(cur);
    } else {
      const a = document.createElement('a');
      a.href = `#/explore/${acc.replace(/\/$/, '')}`;
      a.textContent = part;
      const target = acc;
      a.addEventListener('click', (e) => {
        e.preventDefault();
        navigateToPrefix(target);
      });
      breadcrumbsEl.appendChild(a);
    }
  });

  if (searchQuery) {
    const sep = document.createElement('span');
    sep.className = 'sep';
    sep.textContent = '/';
    breadcrumbsEl.appendChild(sep);
    const cur = document.createElement('span');
    cur.className = 'current';
    cur.textContent = `Search: ${searchQuery}`;
    breadcrumbsEl.appendChild(cur);
  }
}

function folderIcon() {
  return `<svg class="icon folder" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M10 4H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-8l-2-2z"/></svg>`;
}

function fileIcon() {
  return `<svg class="icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6zm1 7V3.5L18.5 9H15z"/></svg>`;
}

function displayName(key, prefix) {
  if (searchQuery) return key;
  if (key.startsWith(prefix)) return key.slice(prefix.length) || key;
  return key;
}

function folderLabel(commonPrefix, prefix) {
  const rest = commonPrefix.startsWith(prefix)
    ? commonPrefix.slice(prefix.length)
    : commonPrefix;
  return rest.replace(/\/$/, '') || commonPrefix;
}

async function refresh() {
  if (!authState.authenticated) return;
  renderBreadcrumbs();
  bucketChip.textContent = currentBucket;
  const params = new URLSearchParams();
  params.set('bucket', currentBucket);
  if (searchQuery) {
    params.set('search', searchQuery);
  } else {
    params.set('prefix', currentPrefix);
    params.set('delimiter', '/');
  }

  let data;
  try {
    const res = await api('/api/v1/objects/list?' + params.toString());
    if (res.status === 401) {
      await bootAuth();
      return;
    }
    if (!res.ok) throw new Error(await res.text());
    data = await res.json();
  } catch (err) {
    showStatus(String(err.message || err), true);
    return;
  }

  listingBody.innerHTML = '';
  closeRowMenu();
  const folderEntries = Array.isArray(data.folders)
    ? data.folders
    : (data.common_prefixes || []).map((prefix) => ({ prefix }));
  const objects = data.objects || [];

  for (const folder of folderEntries) {
    const cp = folder.prefix;
    const tr = document.createElement('tr');
    tr.dataset.kind = 'folder';
    tr.dataset.type = 'folders';
    const name = folderLabel(cp, currentPrefix);
    const etagShort = folder.etag
      ? esc(folder.etag.slice(0, 12)) + '…'
      : '—';
    tr.innerHTML = `
      <td class="col-check">
        <input type="checkbox" data-explorer-key="${escAttr(cp)}" data-kind="folder" data-size="0" aria-label="Select folder ${escAttr(name)}" />
      </td>
      <td>
        <div class="name-cell">
          ${folderIcon()}
          <a class="name-link folder" href="#/explore/${escAttr(cp.replace(/\/$/, ''))}">${esc(name)}</a>
        </div>
      </td>
      <td class="mono">${folder.total_bytes != null ? formatBytes(folder.total_bytes) : '—'}</td>
      <td class="mono">${folder.date_modified ? formatDate(folder.date_modified) : '—'}</td>
      <td class="mono" title="${escAttr(folder.etag || '')}">${etagShort}</td>
      <td>
        <div class="actions">
          <button type="button" class="btn ghost icon-btn" data-act="more" aria-label="More actions" aria-haspopup="menu">⋯</button>
        </div>
      </td>`;
    const items = folderMenuItems(folder);
    tr.querySelector('a').addEventListener('click', (e) => {
      e.preventDefault();
      navigateToPrefix(cp);
    });
    tr.querySelector('input[data-explorer-key]').addEventListener('change', updateExplorerSelectionUi);
    wireRowMenu(tr, items);
    listingBody.appendChild(tr);
  }

  for (const o of objects) {
    const key = o.original_filename;
    const base = key.includes('/') ? key.slice(key.lastIndexOf('/') + 1) : key;
    if (base === '.keep') continue;
    const tr = document.createElement('tr');
    const fileType = classifyFileType(key);
    tr.dataset.kind = 'file';
    tr.dataset.type = fileType;
    const name = displayName(key, currentPrefix);
    tr.innerHTML = `
      <td class="col-check">
        <input type="checkbox" data-explorer-key="${escAttr(key)}" data-kind="file" data-size="${escAttr(String(o.filesize_bytes || 0))}" aria-label="Select ${escAttr(name)}" />
      </td>
      <td>
        <div class="name-cell">
          ${fileIcon()}
          <button type="button" class="name-link file" title="${escAttr(key)}">${esc(name)}</button>
        </div>
      </td>
      <td class="mono">${formatBytes(o.filesize_bytes)}</td>
      <td class="mono">${formatDate(o.date_modified)}</td>
      <td class="mono" title="${escAttr(o.etag)}">${esc((o.etag || '').slice(0, 12))}…</td>
      <td>
        <div class="actions">
          <button type="button" class="btn ghost icon-btn" data-act="more" aria-label="More actions" aria-haspopup="menu">⋯</button>
        </div>
      </td>`;
    const open = () => {
      if (typeof openPreview === 'function') openPreview(key);
      else downloadObject(key, o.filesize_bytes);
    };
    tr.querySelector('.name-link.file').addEventListener('click', open);
    tr.querySelector('input[data-explorer-key]').addEventListener('change', updateExplorerSelectionUi);
    wireRowMenu(tr, fileMenuItems(o, open));
    listingBody.appendChild(tr);
  }

  applyTypeFilter();
}

function downloadObject(key, sizeHint) {
  if (window.MyS3Transfers) {
    window.MyS3Transfers.enqueueDownload(key, sizeHint || 0);
    return;
  }
  // Fallback if transfer module missing
  api(
    '/api/v1/objects/content/' +
      encodeKeyPath(key) +
      '?bucket=' +
      encodeURIComponent(currentBucket),
  )
    .then(async (res) => {
      if (!res.ok) throw new Error(await res.text());
      return res.blob();
    })
    .then((blob) => {
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = key.split('/').pop() || key;
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
    })
    .catch((err) => showStatus(String(err.message || err), true));
}

async function deleteObject(key) {
  const ok = await window.MyS3UI.confirm(`Move “${key}” to recycle bin?`, 'Move to recycle bin');
  if (!ok) return;
  try {
    const res = await api(
      '/api/v1/objects/' + encodeKeyPath(key) + '?bucket=' + encodeURIComponent(currentBucket),
      { method: 'DELETE' },
    );
    if (!res.ok && res.status !== 204) throw new Error(await res.text());
    showStatus(`Moved ${key} to recycle bin`);
    await refresh();
  } catch (err) {
    showStatus(String(err.message || err), true);
  }
}

const rowMenuEl = document.getElementById('row-menu');
/** @type {((e: MouseEvent) => void) | null} */
let rowMenuOutsideHandler = null;

function closeRowMenu() {
  if (!rowMenuEl) return;
  rowMenuEl.hidden = true;
  rowMenuEl.innerHTML = '';
  if (rowMenuOutsideHandler) {
    document.removeEventListener('mousedown', rowMenuOutsideHandler, true);
    rowMenuOutsideHandler = null;
  }
}

/**
 * @param {{ x: number, y: number, items: Array<{ label: string, danger?: boolean, sep?: boolean, action?: () => void }> }} opts
 */
function openRowMenu(opts) {
  closeRowMenu();
  const items = opts.items || [];
  for (const item of items) {
    if (item.sep) {
      const sep = document.createElement('div');
      sep.className = 'row-menu-sep';
      sep.setAttribute('role', 'separator');
      rowMenuEl.appendChild(sep);
      continue;
    }
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'row-menu-item' + (item.danger ? ' danger' : '');
    btn.setAttribute('role', 'menuitem');
    btn.textContent = item.label;
    btn.addEventListener('click', () => {
      closeRowMenu();
      if (typeof item.action === 'function') item.action();
    });
    rowMenuEl.appendChild(btn);
  }
  rowMenuEl.hidden = false;

  const pad = 8;
  const rect = rowMenuEl.getBoundingClientRect();
  let x = opts.x;
  let y = opts.y;
  if (x + rect.width > window.innerWidth - pad) x = window.innerWidth - rect.width - pad;
  if (y + rect.height > window.innerHeight - pad) y = window.innerHeight - rect.height - pad;
  if (x < pad) x = pad;
  if (y < pad) y = pad;
  rowMenuEl.style.left = x + 'px';
  rowMenuEl.style.top = y + 'px';

  rowMenuOutsideHandler = (e) => {
    if (rowMenuEl.contains(e.target)) return;
    if (e.target && e.target.closest && e.target.closest('[data-act="more"]')) return;
    closeRowMenu();
  };
  setTimeout(() => {
    document.addEventListener('mousedown', rowMenuOutsideHandler, true);
  }, 0);
}

function wireRowMenu(tr, items) {
  const moreBtn = tr.querySelector('[data-act="more"]');
  if (moreBtn) {
    moreBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      const r = moreBtn.getBoundingClientRect();
      openRowMenu({ x: r.left, y: r.bottom + 4, items });
    });
  }
  tr.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    openRowMenu({ x: e.clientX, y: e.clientY, items });
  });
}

function folderMenuItems(folder) {
  const cp = folder.prefix;
  return [
    { label: 'Open', action: () => navigateToPrefix(cp) },
    { label: 'Info', action: () => showFolderInfo(folder) },
    { label: 'Rename', action: () => renameFolder(cp) },
    { sep: true },
    { label: 'Delete', danger: true, action: () => deleteFolder(cp) },
  ];
}

function fileMenuItems(o, openFn) {
  const key = o.original_filename;
  return [
    { label: 'Preview', action: openFn },
    { label: 'Info', action: () => showFileInfo(o) },
    { label: 'Download', action: () => downloadObject(key, o.filesize_bytes) },
    { sep: true },
    { label: 'Delete', danger: true, action: () => deleteObject(key) },
  ];
}

function infoFieldRows(rows) {
  return window.MyS3UI.open({
    mode: 'alert',
    title: rows.title,
    renderBody(body) {
      for (const [label, value] of rows.fields) {
        const wrap = document.createElement('label');
        wrap.className = 'field';
        wrap.appendChild(document.createTextNode(label));
        const code = document.createElement('code');
        code.className = 'mono-input';
        code.style.display = 'block';
        code.style.wordBreak = 'break-all';
        code.textContent = value;
        wrap.appendChild(code);
        body.appendChild(wrap);
      }
    },
    buttons: [{ label: 'Close', value: true, primary: true }],
  });
}

function showFolderInfo(folder) {
  const name = folderLabel(folder.prefix, currentPrefix);
  return infoFieldRows({
    title: 'Folder info',
    fields: [
      ['Name', name],
      ['Prefix', folder.prefix],
      ['Created', folder.date_created ? formatDate(folder.date_created) : '—'],
      ['Modified', folder.date_modified ? formatDate(folder.date_modified) : '—'],
      ['Objects', folder.object_count != null ? String(folder.object_count) : '—'],
      ['Total size', folder.total_bytes != null ? formatBytes(folder.total_bytes) : '—'],
      ['ETag', folder.etag || '—'],
    ],
  });
}

function showFileInfo(o) {
  const key = o.original_filename;
  const name = displayName(key, currentPrefix);
  return infoFieldRows({
    title: 'File info',
    fields: [
      ['Name', name],
      ['Key', key],
      ['Created', o.date_uploaded ? formatDate(o.date_uploaded) : '—'],
      ['Modified', o.date_modified ? formatDate(o.date_modified) : '—'],
      ['Size', formatBytes(o.filesize_bytes)],
      ['ETag', o.etag || '—'],
    ],
  });
}

async function deleteFolder(prefix) {
  const name = folderLabel(prefix, currentPrefix);
  const ok = await window.MyS3UI.confirm(
    `Move folder “${name}” and all contents to the recycle bin?`,
    'Delete folder',
  );
  if (!ok) return;
  try {
    const params = new URLSearchParams({
      prefix,
      bucket: currentBucket,
    });
    const res = await api('/api/v1/folders?' + params.toString(), { method: 'DELETE' });
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    showStatus(`Moved folder ${name} (${data.affected || 0} objects) to recycle bin`);
    await refresh();
  } catch (err) {
    showStatus(String(err.message || err), true);
  }
}

async function renameFolder(fromPrefix) {
  const oldName = folderLabel(fromPrefix, currentPrefix);
  const entered = await window.MyS3UI.prompt('New folder name', oldName, 'Rename folder');
  if (entered === null) return;
  const name = entered.trim();
  if (!name || name.includes('/') || name === '.' || name === '..' || name === '.keep') {
    showStatus('Enter a single valid folder name (no /)', true);
    return;
  }
  const toPrefix = currentPrefix + name + '/';
  if (toPrefix === fromPrefix) return;
  try {
    const res = await api('/api/v1/folders/rename', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        bucket: currentBucket,
        from_prefix: fromPrefix,
        to_prefix: toPrefix,
      }),
    });
    if (!res.ok) throw new Error(await res.text());
    showStatus(`Renamed folder to ${name}`);
    if (currentPrefix.startsWith(fromPrefix)) {
      const rest = currentPrefix.slice(fromPrefix.length);
      navigateToPrefix(toPrefix + rest);
    } else {
      await refresh();
    }
  } catch (err) {
    showStatus(String(err.message || err), true);
  }
}

function relativeUploadPath(file) {
  const rel = (file && (file.webkitRelativePath || file.name)) || '';
  return String(rel).replace(/\\/g, '/').replace(/^\/+/, '');
}

async function uploadFiles(fileList) {
  const files = Array.from(fileList || []);
  if (!files.length) return;
  try {
    if (window.MyS3Transfers) {
      window.MyS3Transfers.enqueueUploads(files, currentPrefix);
    } else {
      uploadBtn.disabled = true;
      let ok = 0;
      const dirPrefixes = new Set();
      for (const file of files) {
        const rel = relativeUploadPath(file);
        const parts = rel.split('/');
        if (parts.length > 1) {
          let acc = '';
          for (let i = 0; i < parts.length - 1; i++) {
            acc += parts[i] + '/';
            dirPrefixes.add(acc);
          }
        }
      }
      for (const dir of Array.from(dirPrefixes).sort()) {
        const keepKey = currentPrefix + dir + '.keep';
        const res = await api(
          '/api/v1/objects/' + encodeKeyPath(keepKey) + '?bucket=' + encodeURIComponent(currentBucket),
          {
            method: 'PUT',
            headers: { 'Content-Type': 'application/octet-stream' },
            body: new Blob([]),
          },
        );
        if (!res.ok) throw new Error(`Upload failed for ${keepKey}: ${await res.text()}`);
      }
      for (const file of files) {
        const rel = relativeUploadPath(file);
        if (!rel) continue;
        const key = currentPrefix + rel;
        const res = await api(
          '/api/v1/objects/' + encodeKeyPath(key) + '?bucket=' + encodeURIComponent(currentBucket),
          {
            method: 'PUT',
            headers: { 'Content-Type': file.type || 'application/octet-stream' },
            body: file,
          },
        );
        if (!res.ok) throw new Error(`Upload failed for ${key}: ${await res.text()}`);
        ok += 1;
      }
      showStatus(ok === 1 ? `Uploaded ${relativeUploadPath(files[0])}` : `Uploaded ${ok} files`);
      await refresh();
    }
  } catch (err) {
    showStatus(String(err.message || err), true);
  } finally {
    uploadBtn.disabled = false;
    fileInput.value = '';
    if (folderInput) folderInput.value = '';
  }
}

function closeUploadMenu() {
  if (!uploadMenu || !uploadBtn) return;
  uploadMenu.hidden = true;
  uploadBtn.setAttribute('aria-expanded', 'false');
}

function openUploadMenu() {
  if (!uploadMenu || !uploadBtn) return;
  uploadMenu.hidden = false;
  uploadBtn.setAttribute('aria-expanded', 'true');
}

function toggleUploadMenu() {
  if (!uploadMenu) return;
  if (uploadMenu.hidden) openUploadMenu();
  else closeUploadMenu();
}

async function deleteObjectSilent(key) {
  const res = await api(
    '/api/v1/objects/' + encodeKeyPath(key) + '?bucket=' + encodeURIComponent(currentBucket),
    { method: 'DELETE' },
  );
  if (!res.ok && res.status !== 204) throw new Error(await res.text());
}

async function deleteFolderSilent(prefix) {
  const params = new URLSearchParams({
    prefix,
    bucket: currentBucket,
  });
  const res = await api('/api/v1/folders?' + params.toString(), { method: 'DELETE' });
  if (!res.ok) throw new Error(await res.text());
  return res.json().catch(() => ({}));
}

async function bulkDeleteSelected() {
  const items = selectedExplorerItems();
  if (!items.length) return;
  const ok = await window.MyS3UI.confirmTypeDelete(
    `Move ${items.length} selected item(s) to the recycle bin? Type delete to confirm.`,
    'Delete selected',
  );
  if (!ok) return;
  try {
    for (const item of items) {
      if (item.kind === 'folder') await deleteFolderSilent(item.key);
      else await deleteObjectSilent(item.key);
    }
    showStatus(`Moved ${items.length} item(s) to recycle bin`);
    await refresh();
  } catch (err) {
    showStatus(String(err.message || err), true);
    await refresh();
  }
}

function bulkDownloadSelected() {
  const files = selectedExplorerItems().filter((i) => i.kind === 'file');
  if (!files.length) {
    showStatus('Select one or more files to download', true);
    return;
  }
  if (window.MyS3Transfers) {
    window.MyS3Transfers.enqueueDownloads(files.map((f) => ({ key: f.key, size: f.size })));
  } else {
    files.forEach((f) => downloadObject(f.key, f.size));
  }
}

function validateFolderPath(name) {
  let n = (name || '').trim().replace(/^\/+|\/+$/g, '');
  if (!n) return { error: 'Folder name is required' };
  n = n.replace(/\/+/g, '/');
  const segments = n.split('/');
  for (const seg of segments) {
    if (!seg) return { error: 'Folder path contains an empty segment' };
    if (seg === '.' || seg === '..') return { error: 'Invalid folder name: ' + seg };
    if (seg === '.keep') return { error: 'Folder name cannot be .keep' };
  }
  return { path: n };
}

async function createFolder() {
  const entered = await window.MyS3UI.prompt(
    'Folder path (use / for nested folders, e.g. folder/folder1/folder2)',
    '',
    'New folder',
  );
  if (entered === null) return;
  const result = validateFolderPath(entered);
  if (result.error) {
    showStatus(result.error, true);
    return;
  }
  const path = result.path;
  const key = currentPrefix + path + '/.keep';
  newFolderBtn.disabled = true;
  try {
    const res = await api(
      '/api/v1/objects/' + encodeKeyPath(key) + '?bucket=' + encodeURIComponent(currentBucket),
      {
        method: 'PUT',
        headers: { 'Content-Type': 'application/octet-stream' },
        body: new Blob([]),
      },
    );
    if (!res.ok) throw new Error(await res.text());
    showStatus(`Created folder ${path}`);
    await refresh();
  } catch (e) {
    showStatus(String(e.message || e), true);
  } finally {
    newFolderBtn.disabled = false;
  }
}

function encodeKeyPath(key) {
  return key.split('/').map(encodeURIComponent).join('/');
}

function formatBytes(n) {
  if (n == null) return '—';
  if (n < 1024) return n + ' B';
  if (n < 1024 ** 2) return (n / 1024).toFixed(1) + ' KiB';
  if (n < 1024 ** 3) return (n / 1024 ** 2).toFixed(1) + ' MiB';
  return (n / 1024 ** 3).toFixed(2) + ' GiB';
}

function formatDate(iso) {
  if (!iso) return '—';
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

function esc(s) {
  return String(s)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}

function escAttr(s) {
  return esc(s).replaceAll("'", '&#39;');
}

function showStatus(msg, isError) {
  statusEl.hidden = false;
  statusEl.textContent = msg;
  statusEl.classList.toggle('error', !!isError);
  clearTimeout(showStatus._t);
  showStatus._t = setTimeout(() => {
    statusEl.hidden = true;
  }, 4000);
}

async function bootAuth() {
  const res = await api('/api/v1/auth/status');
  if (!res.ok) {
    showAuthStatus(await res.text(), true);
    authGate.hidden = false;
    appShell.hidden = true;
    return;
  }
  authState = await res.json();
  if (authState.authenticated) {
    authGate.hidden = true;
    appShell.hidden = false;
    setBucket(currentBucket);
    if (!location.hash || location.hash === '#/' || location.hash === '#') {
      location.hash = '#/explore/';
    } else {
      applyRoute();
    }
    if (window.MyS3Settings) window.MyS3Settings.onAuth(authState);
    return;
  }

  appShell.hidden = true;
  authGate.hidden = false;
  credReveal.hidden = true;
  if (authState.needs_bootstrap) {
    authBootstrap.hidden = false;
    authLogin.hidden = true;
  } else {
    authBootstrap.hidden = true;
    authLogin.hidden = false;
  }
}

document.getElementById('bootstrap-btn').addEventListener('click', async () => {
  const display_name = document.getElementById('bootstrap-name').value.trim() || 'Owner';
  try {
    const res = await fetch('/api/v1/auth/bootstrap', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ display_name }),
    });
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    setToken(data.session_token);
    showCreds(data.credentials.username_hex, data.credentials.password_hex);
  } catch (err) {
    showAuthStatus(String(err.message || err), true);
  }
});

document.getElementById('login-btn').addEventListener('click', async () => {
  const username_hex = document.getElementById('login-user').value.trim();
  const password_hex = document.getElementById('login-pass').value.trim();
  try {
    const res = await fetch('/api/v1/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username_hex, password_hex }),
    });
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    setToken(data.session_token);
    await bootAuth();
  } catch (err) {
    showAuthStatus(String(err.message || err), true);
  }
});

document.getElementById('cred-done').addEventListener('click', () => bootAuth());

document.querySelectorAll('[data-copy]').forEach((btn) => {
  btn.addEventListener('click', async () => {
    const id = btn.getAttribute('data-copy');
    const text = document.getElementById(id).textContent;
    try {
      await navigator.clipboard.writeText(text);
      showAuthStatus('Copied');
    } catch {
      showAuthStatus('Copy failed', true);
    }
  });
});

document.getElementById('logout-btn').addEventListener('click', async () => {
  await api('/api/v1/auth/logout', { method: 'POST' });
  setToken('');
  await bootAuth();
});

uploadBtn.addEventListener('click', (e) => {
  e.stopPropagation();
  toggleUploadMenu();
});
if (uploadMenu) {
  uploadMenu.addEventListener('click', (e) => {
    const item = e.target.closest('[data-upload]');
    if (!item) return;
    e.stopPropagation();
    closeUploadMenu();
    const mode = item.getAttribute('data-upload');
    if (mode === 'folder' && folderInput) folderInput.click();
    else fileInput.click();
  });
}
document.addEventListener('mousedown', (e) => {
  if (!uploadMenu || uploadMenu.hidden) return;
  if (uploadBtn.contains(e.target) || uploadMenu.contains(e.target)) return;
  closeUploadMenu();
});
fileInput.addEventListener('change', () => uploadFiles(fileInput.files));
if (folderInput) {
  folderInput.addEventListener('change', () => uploadFiles(folderInput.files));
}
newFolderBtn.addEventListener('click', () => {
  createFolder().catch((e) => showStatus(String(e.message || e), true));
});

searchInput.addEventListener('input', () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    searchQuery = searchInput.value.trim();
    refresh();
  }, 250);
});

document.getElementById('filter-bar').addEventListener('click', (e) => {
  const btn = e.target.closest('[data-filter]');
  if (!btn) return;
  typeFilter = btn.getAttribute('data-filter') || 'all';
  document.querySelectorAll('#filter-bar .filter-chip').forEach((el) => {
    el.classList.toggle('active', el === btn);
  });
  applyTypeFilter();
});

document.getElementById('listing-select-all').addEventListener('change', (e) => {
  const on = !!e.target.checked;
  visibleExplorerChecks().forEach((box) => {
    box.checked = on;
  });
  updateExplorerSelectionUi();
});

document.getElementById('explorer-download-selected').addEventListener('click', () => {
  bulkDownloadSelected();
});

document.getElementById('explorer-delete-selected').addEventListener('click', () => {
  bulkDeleteSelected().catch((e) => showStatus(String(e.message || e), true));
});

window.addEventListener('hashchange', () => {
  closeRowMenu();
  if (!authState.authenticated) return;
  applyRoute();
});

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (uploadMenu && !uploadMenu.hidden) {
    closeUploadMenu();
    return;
  }
  if (rowMenuEl && !rowMenuEl.hidden) {
    closeRowMenu();
  }
});

document.getElementById('brand-link').addEventListener('click', (e) => {
  e.preventDefault();
  navigateToPrefix('');
});

window.MyS3 = {
  api,
  authHeaders,
  getToken,
  setToken,
  getBucket: () => currentBucket,
  setBucket,
  refresh,
  downloadObject,
  encodeKeyPath,
  formatBytes,
  formatDate,
  esc,
  escAttr,
  showStatus,
  getAuth: () => authState,
  bootAuth,
};

bootAuth();
