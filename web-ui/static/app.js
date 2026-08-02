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
const viewActions = document.getElementById('view-actions');
const viewSettings = document.getElementById('view-settings');
const tabExplore = document.getElementById('tab-explore');
const tabActions = document.getElementById('tab-actions');
const tabSettings = document.getElementById('tab-settings');
const searchLoadingEl = document.getElementById('search-loading');

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
const EXPLORER_PAGE_SIZE = 50;
let explorerOffset = 0;
let explorerHasMore = false;
let explorerLoadingMore = false;
let explorerTotal = 0;
/** @type {IntersectionObserver|null} */
let explorerScrollObserver = null;

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
  if (parts[0] === 'actions') {
    return { view: 'actions', panel: null, prefix: '' };
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
  const onActions = route.view === 'actions';
  const onExplore = route.view === 'explore';
  viewExplore.hidden = !onExplore;
  if (viewActions) viewActions.hidden = !onActions;
  viewSettings.hidden = !onSettings;
  tabExplore.classList.toggle('active', onExplore);
  if (tabActions) tabActions.classList.toggle('active', onActions);
  tabSettings.classList.toggle('active', onSettings);

  if (!onActions && window.MyS3Actions && typeof window.MyS3Actions.hide === 'function') {
    window.MyS3Actions.hide();
  }

  if (onSettings) {
    if (window.MyS3Settings) window.MyS3Settings.showPanel(route.panel);
    return;
  }
  if (onActions) {
    if (window.MyS3Actions && typeof window.MyS3Actions.show === 'function') {
      window.MyS3Actions.show();
    }
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
const VIDEO_EXT = new Set([
  'mp4', 'webm', 'mov', 'mkv', 'avi', 'm4v', 'ogg', 'ogv',
  'wmv', 'flv', 'mpeg', 'mpg', 'mpe', '3gp', '3g2',
  'ts', 'm2ts', 'mts', 'vob', 'asf', 'f4v', 'rm', 'rmvb',
]);
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

function appendFolderRow(folder) {
  const cp = folder.prefix;
  const tr = document.createElement('tr');
  tr.dataset.kind = 'folder';
  tr.dataset.type = 'folders';
  const name = folderLabel(cp, currentPrefix);
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
    <td class="mono" title="Folders do not have ETags">—</td>
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

function appendObjectRow(o) {
  const key = o.original_filename;
  const base = key.includes('/') ? key.slice(key.lastIndexOf('/') + 1) : key;
  if (base === '.keep') return;
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

function updateExplorerScrollFooter() {
  const footer = document.getElementById('explorer-scroll-footer');
  const status = document.getElementById('explorer-scroll-status');
  const spinner = document.getElementById('explorer-scroll-spinner');
  if (!footer || !status) return;
  const loaded = listingBody.querySelectorAll('tr[data-kind]').length;
  if (explorerTotal <= 0 && loaded === 0) {
    footer.hidden = true;
    return;
  }
  footer.hidden = false;
  if (spinner) spinner.hidden = !explorerLoadingMore;
  if (explorerLoadingMore) {
    status.textContent = 'Loading more…';
  } else if (explorerHasMore) {
    status.textContent = `Showing ${loaded} of ${explorerTotal} · scroll for more`;
  } else {
    status.textContent = explorerTotal > 0 ? `Showing all ${explorerTotal}` : '';
    if (!status.textContent) footer.hidden = true;
  }
}

function ensureExplorerScrollObserver() {
  const footer = document.getElementById('explorer-scroll-footer');
  if (!footer) return;
  if (explorerScrollObserver) return;
  explorerScrollObserver = new IntersectionObserver(
    (entries) => {
      if (!entries.some((e) => e.isIntersecting)) return;
      if (!explorerHasMore || explorerLoadingMore) return;
      loadExplorerPage({ append: true }).catch((err) => {
        showStatus(String(err.message || err), true);
      });
    },
    { root: null, rootMargin: '240px 0px', threshold: 0 },
  );
  explorerScrollObserver.observe(footer);
}

async function loadExplorerPage({ append }) {
  if (!authState.authenticated) return;
  if (append) {
    if (!explorerHasMore || explorerLoadingMore) return;
    explorerLoadingMore = true;
    updateExplorerScrollFooter();
  } else {
    explorerOffset = 0;
    explorerHasMore = false;
    explorerTotal = 0;
    explorerLoadingMore = true;
    listingBody.innerHTML = '';
    closeRowMenu();
    clearExplorerSelection();
    updateExplorerScrollFooter();
  }

  renderBreadcrumbs();
  bucketChip.textContent = currentBucket;
  const params = new URLSearchParams();
  params.set('bucket', currentBucket);
  params.set('limit', String(EXPLORER_PAGE_SIZE));
  params.set('offset', String(append ? explorerOffset : 0));
  if (searchQuery) {
    params.set('search', searchQuery);
  } else {
    params.set('prefix', currentPrefix);
    params.set('delimiter', '/');
  }

  if (searchLoadingEl) searchLoadingEl.hidden = !searchQuery || append;
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
  } finally {
    if (searchLoadingEl) searchLoadingEl.hidden = true;
    explorerLoadingMore = false;
  }

  const folderEntries = Array.isArray(data.folders)
    ? data.folders
    : (data.common_prefixes || []).map((prefix) => ({ prefix }));
  const objects = data.objects || [];
  for (const folder of folderEntries) appendFolderRow(folder);
  for (const o of objects) appendObjectRow(o);

  const returned = folderEntries.length + objects.filter((o) => {
    const base = o.original_filename.includes('/')
      ? o.original_filename.slice(o.original_filename.lastIndexOf('/') + 1)
      : o.original_filename;
    return base !== '.keep';
  }).length;
  explorerOffset = (append ? explorerOffset : 0) + returned;
  explorerTotal = Number(data.total) || explorerOffset;
  explorerHasMore = !!data.has_more;
  ensureExplorerScrollObserver();
  updateExplorerScrollFooter();
  applyTypeFilter();
}

async function refresh() {
  await loadExplorerPage({ append: false });
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
    {
      label: 'Download…',
      action: () => {
        downloadFolder(cp).catch((e) => showStatus(String(e.message || e), true));
      },
    },
    { label: 'Share', action: () => openShareDialog({ kind: 'folder', key: cp }) },
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
    { label: 'Share', action: () => openShareDialog({ kind: 'file', key }) },
    { sep: true },
    { label: 'Delete', danger: true, action: () => deleteObject(key) },
  ];
}

function accessModeLabel(mode) {
  if (mode === 'public') return 'Public';
  if (mode === 'bucket_readers') return 'Bucket readers';
  if (mode === 'specific_users') return 'Specific users';
  return mode;
}

function formatShareExpiry(expiresAt) {
  if (!expiresAt) return 'Never';
  return formatDate(expiresAt);
}

async function openShareDialog(target) {
  const kind = target.kind;
  const key = target.key;

  let directory = [];
  try {
    const res = await api('/api/v1/accounts/directory');
    if (res.ok) directory = await res.json();
  } catch {
    directory = [];
  }

  let keepOpen = true;
  while (keepOpen) {
    /** @type {HTMLSelectElement | null} */
    let modeSelect = null;
    /** @type {HTMLSelectElement | null} */
    let expirySelect = null;
    /** @type {HTMLInputElement | null} */
    let customExpiry = null;
    /** @type {HTMLInputElement | null} */
    let shortenCheck = null;
    /** @type {HTMLElement | null} */
    let usersWrap = null;
    /** @type {HTMLSelectElement | null} */
    let usersSelect = null;
    /** @type {HTMLElement | null} */
    let existingList = null;
    /** @type {HTMLElement | null} */
    let createdWrap = null;
    /** @type {string | null} */
    let pendingCreateError = null;

    const syncModeUi = () => {
      if (usersWrap && modeSelect) usersWrap.hidden = modeSelect.value !== 'specific_users';
    };
    const syncExpiryUi = () => {
      if (customExpiry && expirySelect) customExpiry.hidden = expirySelect.value !== 'custom';
    };

    const loadExisting = async () => {
      if (!existingList) return;
      existingList.innerHTML = '<p class="muted">Loading…</p>';
      try {
        const res = await api(
          '/api/v1/shares?bucket=' +
            encodeURIComponent(currentBucket) +
            '&key=' +
            encodeURIComponent(key),
        );
        if (!res.ok) throw new Error(await res.text());
        const rows = await res.json();
        existingList.innerHTML = '';
        if (!rows.length) {
          const p = document.createElement('p');
          p.className = 'muted';
          p.textContent = 'No active shares yet.';
          existingList.appendChild(p);
          return;
        }
        for (const row of rows) {
          const item = document.createElement('div');
          item.className = 'share-existing-item';
          const info = document.createElement('div');
          info.className = 'share-existing-meta';
          const url = location.origin + row.url_path;
          const code = document.createElement('code');
          code.className = 'mono-input';
          code.textContent = url;
          const meta = document.createElement('span');
          meta.className = 'muted';
          meta.textContent =
            accessModeLabel(row.access_mode) + ' · Expires ' + formatShareExpiry(row.expires_at);
          info.appendChild(code);
          info.appendChild(meta);
          const actions = document.createElement('div');
          actions.className = 'share-existing-actions';
          const copyBtn = document.createElement('button');
          copyBtn.type = 'button';
          copyBtn.className = 'btn ghost';
          copyBtn.textContent = 'Copy';
          copyBtn.addEventListener('click', async () => {
            try {
              await navigator.clipboard.writeText(url);
              copyBtn.textContent = 'Copied';
              setTimeout(() => {
                copyBtn.textContent = 'Copy';
              }, 1500);
            } catch {
              copyBtn.textContent = 'Failed';
            }
          });
          const revokeBtn = document.createElement('button');
          revokeBtn.type = 'button';
          revokeBtn.className = 'btn ghost danger';
          revokeBtn.textContent = 'Revoke';
          revokeBtn.addEventListener('click', async () => {
            // Native confirm — nested MyS3UI modals would close this dialog.
            if (!window.confirm('Revoke this share link?')) return;
            const del = await api('/api/v1/shares/' + row.id, { method: 'DELETE' });
            if (!del.ok && del.status !== 204) {
              showStatus(await del.text(), true);
              return;
            }
            await loadExisting();
          });
          actions.appendChild(copyBtn);
          actions.appendChild(revokeBtn);
          item.appendChild(info);
          item.appendChild(actions);
          existingList.appendChild(item);
        }
      } catch (err) {
        existingList.textContent = String(err.message || err);
      }
    };

    const action = await window.MyS3UI.open({
      mode: 'custom',
      title: 'Share ' + (kind === 'folder' ? 'folder' : 'file'),
      renderBody(body) {
        const panel = body.closest('.ui-modal-panel');
        if (panel) panel.classList.add('ui-modal-panel-wide');

        const lead = document.createElement('p');
        lead.className = 'ui-modal-message';
        lead.textContent = key;
        body.appendChild(lead);

        if (pendingCreateError) {
          const errEl = document.createElement('p');
          errEl.className = 'status error';
          errEl.textContent = pendingCreateError;
          body.appendChild(errEl);
        }

        const modeField = document.createElement('label');
        modeField.className = 'field';
        modeField.appendChild(document.createTextNode('Who can access'));
        modeSelect = document.createElement('select');
        [
          ['public', 'Public (anyone with the link)'],
          ['bucket_readers', 'Anyone with read permission on this bucket'],
          ['specific_users', 'Specific users'],
        ].forEach(([value, label]) => {
          const opt = document.createElement('option');
          opt.value = value;
          opt.textContent = label;
          modeSelect.appendChild(opt);
        });
        modeSelect.addEventListener('change', syncModeUi);
        modeField.appendChild(modeSelect);
        body.appendChild(modeField);

        usersWrap = document.createElement('label');
        usersWrap.className = 'field';
        usersWrap.hidden = true;
        usersWrap.appendChild(document.createTextNode('Users'));
        usersSelect = document.createElement('select');
        usersSelect.multiple = true;
        usersSelect.size = Math.min(6, Math.max(3, directory.length || 3));
        usersSelect.className = 'share-users-select';
        for (const a of directory) {
          const opt = document.createElement('option');
          opt.value = String(a.id);
          opt.textContent = a.display_name || '#' + a.id;
          usersSelect.appendChild(opt);
        }
        usersWrap.appendChild(usersSelect);
        body.appendChild(usersWrap);

        const expiryField = document.createElement('label');
        expiryField.className = 'field';
        expiryField.appendChild(document.createTextNode('Expiry'));
        expirySelect = document.createElement('select');
        [
          ['never', 'Never'],
          ['1h', '1 hour'],
          ['1d', '1 day'],
          ['7d', '7 days'],
          ['30d', '30 days'],
          ['custom', 'Custom…'],
        ].forEach(([value, label]) => {
          const opt = document.createElement('option');
          opt.value = value;
          opt.textContent = label;
          expirySelect.appendChild(opt);
        });
        expirySelect.addEventListener('change', syncExpiryUi);
        expiryField.appendChild(expirySelect);
        body.appendChild(expiryField);

        customExpiry = document.createElement('input');
        customExpiry.type = 'datetime-local';
        customExpiry.className = 'ui-modal-input';
        customExpiry.hidden = true;
        body.appendChild(customExpiry);

        const shortenField = document.createElement('label');
        shortenField.className = 'field inline share-shorten';
        shortenCheck = document.createElement('input');
        shortenCheck.type = 'checkbox';
        shortenField.appendChild(shortenCheck);
        shortenField.appendChild(document.createTextNode(' Shorten link'));
        body.appendChild(shortenField);

        createdWrap = document.createElement('div');
        createdWrap.className = 'share-created';
        createdWrap.hidden = true;
        body.appendChild(createdWrap);

        const existingTitle = document.createElement('p');
        existingTitle.className = 'share-section-title';
        existingTitle.textContent = 'Active shares';
        body.appendChild(existingTitle);
        existingList = document.createElement('div');
        existingList.className = 'share-existing-list';
        body.appendChild(existingList);

        // Fire-and-forget load while modal is visible.
        queueMicrotask(() => {
          loadExisting();
        });
      },
      buttons: [
        { label: 'Close', value: 'close' },
        {
          label: 'Create link',
          primary: true,
          getValue: () => {
            const mode = modeSelect ? modeSelect.value : 'public';
            const account_ids = [];
            if (mode === 'specific_users' && usersSelect) {
              for (const opt of usersSelect.selectedOptions) {
                account_ids.push(Number(opt.value));
              }
            }
            let expires_at = null;
            const exp = expirySelect ? expirySelect.value : 'never';
            if (exp === '1h') expires_at = new Date(Date.now() + 3600e3).toISOString();
            else if (exp === '1d') expires_at = new Date(Date.now() + 86400e3).toISOString();
            else if (exp === '7d') expires_at = new Date(Date.now() + 7 * 86400e3).toISOString();
            else if (exp === '30d') expires_at = new Date(Date.now() + 30 * 86400e3).toISOString();
            else if (exp === 'custom') {
              if (!customExpiry || !customExpiry.value) return { error: 'Pick a custom expiry time' };
              expires_at = new Date(customExpiry.value).toISOString();
            }
            return {
              action: 'create',
              access_mode: mode,
              account_ids,
              expires_at,
              shorten: !!(shortenCheck && shortenCheck.checked),
            };
          },
        },
      ],
    });

    if (!action || action === 'close') {
      keepOpen = false;
      break;
    }
    if (action.error) {
      await window.MyS3UI.alert(action.error, 'Share');
      continue;
    }
    if (action.action === 'create') {
      if (action.access_mode === 'specific_users' && !action.account_ids.length) {
        await window.MyS3UI.alert('Select at least one user.', 'Share');
        continue;
      }
      try {
        const res = await api('/api/v1/shares', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            bucket: currentBucket,
            key,
            kind,
            access_mode: action.access_mode,
            account_ids: action.account_ids,
            expires_at: action.expires_at,
            shorten: action.shorten,
          }),
        });
        if (!res.ok) throw new Error(await res.text());
        const created = await res.json();
        const url = location.origin + created.url_path;
        try {
          await navigator.clipboard.writeText(url);
          showStatus('Share link created and copied');
        } catch {
          showStatus('Share link created');
        }
        await window.MyS3UI.open({
          mode: 'alert',
          title: 'Share link created',
          renderBody(body) {
            const p = document.createElement('p');
            p.className = 'ui-modal-message';
            p.textContent = 'Anyone allowed by the access setting can open this link.';
            body.appendChild(p);
            const wrap = document.createElement('label');
            wrap.className = 'field';
            wrap.appendChild(document.createTextNode('Link'));
            const copyRow = document.createElement('div');
            copyRow.className = 'copy-row';
            const code = document.createElement('code');
            code.textContent = url;
            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'btn ghost';
            btn.textContent = 'Copy';
            btn.addEventListener('click', async () => {
              try {
                await navigator.clipboard.writeText(url);
                btn.textContent = 'Copied';
                setTimeout(() => {
                  btn.textContent = 'Copy';
                }, 1500);
              } catch {
                btn.textContent = 'Failed';
              }
            });
            copyRow.appendChild(code);
            copyRow.appendChild(btn);
            wrap.appendChild(copyRow);
            body.appendChild(wrap);
          },
          buttons: [{ label: 'Done', value: true, primary: true }],
        });
        // Re-open manage dialog so user can revoke / create more.
        continue;
      } catch (err) {
        pendingCreateError = String(err.message || err);
        await window.MyS3UI.alert(pendingCreateError, 'Share failed');
        continue;
      }
    }
    keepOpen = false;
  }
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

function formatEtagAlgo(value) {
  if (!value) return '—';
  const s = String(value);
  const labels = {
    md5: 'MD5',
    sha256: 'SHA-256',
    sha512: 'SHA-512',
    'blake2-128': 'Blake2-128',
    'blake2-256': 'Blake2-256',
    'blake3-128': 'Blake3-128',
    'blake3-256': 'Blake3-256',
  };
  return labels[s] || s;
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
      ['ETag algorithm', formatEtagAlgo(o.etag_type)],
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

async function pickArchiveFormat() {
  /** @type {HTMLSelectElement | null} */
  let select = null;
  const format = await window.MyS3UI.open({
    mode: 'custom',
    title: 'Download folder',
    renderBody(body) {
      const p = document.createElement('p');
      p.className = 'ui-modal-message';
      p.textContent = 'Choose archive format';
      body.appendChild(p);
      const label = document.createElement('label');
      label.className = 'field';
      label.appendChild(document.createTextNode('Format'));
      select = document.createElement('select');
      select.className = 'ui-modal-input';
      [
        ['zip', 'ZIP (.zip)'],
        ['tar.gz', 'Gzip tar (.tar.gz)'],
        ['7z', '7-Zip (.7z)'],
      ].forEach(([value, text]) => {
        const opt = document.createElement('option');
        opt.value = value;
        opt.textContent = text;
        select.appendChild(opt);
      });
      label.appendChild(select);
      body.appendChild(label);
    },
    buttons: [
      { label: 'Cancel', value: null },
      {
        label: 'Download',
        primary: true,
        getValue: () => (select ? select.value : 'zip'),
      },
    ],
  });
  return format;
}

/** True if the folder has any non-`.keep` objects (recursive). */
async function folderHasDownloadableFiles(prefix) {
  const params = new URLSearchParams({
    prefix,
    delimiter: '',
    bucket: currentBucket,
    limit: '1',
    offset: '0',
  });
  const res = await api('/api/v1/objects/list?' + params.toString());
  if (!res.ok) return true;
  const data = await res.json();
  return !!(data.objects && data.objects.length > 0);
}

async function downloadFolder(prefix) {
  const format = await pickArchiveFormat();
  if (format == null) return;
  const hasFiles = await folderHasDownloadableFiles(prefix);
  if (!hasFiles) {
    showStatus('Folder is empty', true);
    return;
  }
  if (window.MyS3Transfers && window.MyS3Transfers.enqueueFolderDownload) {
    window.MyS3Transfers.enqueueFolderDownload(prefix, format);
  } else {
    showStatus('Transfers unavailable', true);
  }
}

async function bulkDownloadSelected() {
  const items = selectedExplorerItems();
  const files = items.filter((i) => i.kind === 'file');
  const folders = items.filter((i) => i.kind === 'folder');
  if (!files.length && !folders.length) {
    showStatus('Select one or more items to download', true);
    return;
  }

  let format = null;
  if (folders.length) {
    format = await pickArchiveFormat();
    if (format == null) return;
  }

  if (files.length) {
    if (window.MyS3Transfers) {
      window.MyS3Transfers.enqueueDownloads(files.map((f) => ({ key: f.key, size: f.size })));
    } else {
      files.forEach((f) => downloadObject(f.key, f.size));
    }
  }

  if (!folders.length) return;
  if (!window.MyS3Transfers || !window.MyS3Transfers.enqueueFolderDownload) {
    showStatus('Transfers unavailable', true);
    return;
  }

  let skipped = 0;
  for (const folder of folders) {
    const hasFiles = await folderHasDownloadableFiles(folder.key);
    if (!hasFiles) {
      skipped += 1;
      continue;
    }
    window.MyS3Transfers.enqueueFolderDownload(folder.key, format);
  }
  if (skipped) {
    showStatus(
      skipped === folders.length
        ? 'Selected folder(s) are empty'
        : `Skipped ${skipped} empty folder(s)`,
      true,
    );
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
  try {
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
      if (window.MyS3Actions) window.MyS3Actions.onAuth(authState);
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
  } catch (err) {
    showAuthStatus(String(err && err.message ? err.message : err), true);
    authGate.hidden = false;
    appShell.hidden = true;
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

function fileWithRelativePath(file, relativePath) {
  const rel = String(relativePath || '').replace(/\\/g, '/').replace(/^\/+/, '');
  try {
    Object.defineProperty(file, 'webkitRelativePath', {
      configurable: true,
      value: rel,
    });
    return file;
  } catch {
    const copy = new File([file], file.name, {
      type: file.type,
      lastModified: file.lastModified,
    });
    try {
      Object.defineProperty(copy, 'webkitRelativePath', {
        configurable: true,
        value: rel,
      });
    } catch {
      /* ignore */
    }
    return copy;
  }
}

function readDirectoryEntries(dirReader) {
  return new Promise((resolve, reject) => {
    const all = [];
    const readBatch = () => {
      dirReader.readEntries((entries) => {
        if (!entries.length) {
          resolve(all);
          return;
        }
        all.push(...entries);
        readBatch();
      }, reject);
    };
    readBatch();
  });
}

async function walkFileEntry(entry, pathPrefix, out) {
  if (!entry) return;
  if (entry.isFile) {
    const file = await new Promise((resolve, reject) => entry.file(resolve, reject));
    const rel = (pathPrefix + entry.name).replace(/^\/+/, '');
    out.push(fileWithRelativePath(file, rel));
    return;
  }
  if (entry.isDirectory) {
    const reader = entry.createReader();
    const children = await readDirectoryEntries(reader);
    const nextPrefix = pathPrefix + entry.name + '/';
    for (const child of children) {
      await walkFileEntry(child, nextPrefix, out);
    }
  }
}

async function collectDroppedFiles(dataTransfer) {
  const out = [];
  const items = dataTransfer && dataTransfer.items;
  if (items && items.length) {
    let usedEntries = false;
    const walks = [];
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (item.kind !== 'file') continue;
      const entry =
        typeof item.webkitGetAsEntry === 'function' ? item.webkitGetAsEntry() : null;
      if (entry) {
        usedEntries = true;
        walks.push(walkFileEntry(entry, '', out));
      }
    }
    if (usedEntries) {
      await Promise.all(walks);
      return out;
    }
  }
  return Array.from((dataTransfer && dataTransfer.files) || []);
}

function dtHasFiles(dt) {
  if (!dt || !dt.types) return false;
  for (let i = 0; i < dt.types.length; i++) {
    if (dt.types[i] === 'Files') return true;
  }
  return false;
}

function setExplorerDropActive(on) {
  if (!viewExplore) return;
  viewExplore.classList.toggle('explorer-drop-active', !!on);
  const overlay = document.getElementById('explorer-drop-overlay');
  if (overlay) overlay.hidden = !on;
}

let explorerDragDepth = 0;
if (viewExplore) {
  viewExplore.addEventListener('dragenter', (e) => {
    if (viewExplore.hidden || !dtHasFiles(e.dataTransfer)) return;
    const t = e.target;
    if (t && (t.closest('input, textarea, select, [contenteditable="true"]'))) return;
    e.preventDefault();
    explorerDragDepth += 1;
    setExplorerDropActive(true);
  });
  viewExplore.addEventListener('dragover', (e) => {
    if (viewExplore.hidden || !dtHasFiles(e.dataTransfer)) return;
    const t = e.target;
    if (t && (t.closest('input, textarea, select, [contenteditable="true"]'))) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
    setExplorerDropActive(true);
  });
  viewExplore.addEventListener('dragleave', (e) => {
    if (!dtHasFiles(e.dataTransfer)) return;
    explorerDragDepth = Math.max(0, explorerDragDepth - 1);
    if (explorerDragDepth === 0) setExplorerDropActive(false);
  });
  viewExplore.addEventListener('drop', (e) => {
    const t = e.target;
    if (t && (t.closest('input, textarea, select, [contenteditable="true"]'))) return;
    if (!dtHasFiles(e.dataTransfer)) return;
    e.preventDefault();
    e.stopPropagation();
    explorerDragDepth = 0;
    setExplorerDropActive(false);
    collectDroppedFiles(e.dataTransfer)
      .then((files) => {
        if (!files.length) {
          showStatus('No files to upload', true);
          return;
        }
        return uploadFiles(files);
      })
      .catch((err) => showStatus(String(err.message || err), true));
  });
}

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
  bulkDownloadSelected().catch((e) => showStatus(String(e.message || e), true));
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
