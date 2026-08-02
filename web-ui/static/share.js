(function () {
  const SESSION_KEY = 'mys3_session_token';
  const IMAGE_EXT = new Set([
    'jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp', 'svg', 'svgz', 'ico', 'avif', 'jfif', 'pjpeg', 'pjp',
  ]);
  const VIDEO_EXT = new Set([
    'mp4', 'webm', 'ogg', 'ogv', 'mov', 'm4v', 'mkv',
    'avi', 'wmv', 'flv', 'mpeg', 'mpg', 'mpe', '3gp', '3g2',
    'ts', 'm2ts', 'mts', 'vob', 'asf', 'f4v', 'rm', 'rmvb',
  ]);
  const TEXT_EXT = new Set([
    'txt', 'text', 'log', 'csv', 'tsv', 'json', 'jsonc', 'md', 'markdown', 'mdown', 'mkd',
    'js', 'mjs', 'cjs', 'ts', 'tsx', 'jsx', 'py', 'rs', 'go', 'java', 'c', 'h', 'cpp', 'hpp',
    'cs', 'rb', 'php', 'swift', 'sh', 'bash', 'zsh', 'sql', 'html', 'htm', 'xml', 'css', 'scss',
    'yml', 'yaml', 'toml', 'ini', 'env', 'dockerfile', 'makefile',
  ]);

  const gateEl = document.getElementById('share-gate');
  const loginEl = document.getElementById('share-login');
  const loginLead = document.getElementById('share-login-lead');
  const errorEl = document.getElementById('share-error');
  const errorTitle = document.getElementById('share-error-title');
  const errorMsg = document.getElementById('share-error-msg');
  const mainEl = document.getElementById('share-main');
  const titleEl = document.getElementById('share-title');
  const metaEl = document.getElementById('share-meta');
  const kickerEl = document.getElementById('share-kicker');
  const chipEl = document.getElementById('share-chip');
  const heroIcon = document.getElementById('share-hero-icon');
  const fileActions = document.getElementById('share-file-actions');
  const previewBtn = document.getElementById('share-preview-btn');
  const downloadBtn = document.getElementById('share-download-btn');
  const fileStage = document.getElementById('share-file-stage');
  const stageLoading = document.getElementById('share-stage-loading');
  const stageBody = document.getElementById('share-stage-body');
  const stageFallback = document.getElementById('share-stage-fallback');
  const stageDownload = document.getElementById('share-stage-download');
  const folderView = document.getElementById('share-folder-view');
  const breadcrumbsEl = document.getElementById('share-breadcrumbs');
  const listingBody = document.getElementById('share-listing-body');
  const emptyEl = document.getElementById('share-empty');
  const loginStatus = document.getElementById('share-login-status');
  const toastEl = document.getElementById('share-toast');

  /** @type {'token'|'code'|null} */
  let idKind = null;
  /** @type {string} */
  let idValue = '';
  /** @type {string} Signed access query for non-public share page links. */
  let accessToken = '';
  /** @type {string} */
  let shareRoot = '';
  /** @type {string} */
  let browsePrefix = '';
  /** @type {object|null} */
  let meta = null;
  /** @type {string|null} */
  let stageObjectUrl = null;

  function getToken() {
    return localStorage.getItem(SESSION_KEY) || '';
  }

  function setToken(token) {
    if (token) localStorage.setItem(SESSION_KEY, token);
    else localStorage.removeItem(SESSION_KEY);
  }

  function authHeaders(extra) {
    const h = Object.assign({}, extra || {});
    const t = getToken();
    if (t) h['Authorization'] = 'Bearer ' + t;
    return h;
  }

  function apiBase() {
    if (idKind === 'code') return '/api/v1/shares/by-code/' + encodeURIComponent(idValue);
    return '/api/v1/shares/by-token/' + encodeURIComponent(idValue);
  }

  function withAccess(url) {
    if (!accessToken) return url;
    const sep = url.indexOf('?') >= 0 ? '&' : '?';
    return url + sep + 'access=' + encodeURIComponent(accessToken);
  }

  function encodeKeyPath(key) {
    return key
      .split('/')
      .map((p) => encodeURIComponent(p))
      .join('/');
  }

  function contentUrl(key) {
    return withAccess(apiBase() + '/content/' + encodeKeyPath(key));
  }

  function api(path, opts) {
    const options = Object.assign({ headers: {} }, opts || {});
    options.headers = authHeaders(options.headers);
    return fetch(path, options);
  }

  function formatBytes(n) {
    const v = Number(n) || 0;
    if (v < 1024) return v + ' B';
    const units = ['KB', 'MB', 'GB', 'TB'];
    let x = v;
    let i = -1;
    do {
      x /= 1024;
      i += 1;
    } while (x >= 1024 && i < units.length - 1);
    return x.toFixed(x >= 10 || i === 0 ? 0 : 1) + ' ' + units[i];
  }

  function formatDate(iso) {
    if (!iso) return '—';
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return String(iso);
    }
  }

  function esc(s) {
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function escAttr(s) {
    return esc(s).replace(/'/g, '&#39;');
  }

  function fileExt(key) {
    const base = (key.split('/').pop() || '').toLowerCase();
    if (base === 'dockerfile' || base === 'makefile') return base;
    const i = base.lastIndexOf('.');
    if (i <= 0) return '';
    return base.slice(i + 1);
  }

  function isPreviewable(key) {
    if (typeof window.isPreviewable === 'function') return window.isPreviewable(key);
    const ext = fileExt(key);
    return IMAGE_EXT.has(ext) || VIDEO_EXT.has(ext) || TEXT_EXT.has(ext);
  }

  function folderIconSvg() {
    return `<svg class="icon folder" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M10 4H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-8l-2-2z"/></svg>`;
  }

  function fileIconSvg() {
    return `<svg class="icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6zm1 7V3.5L18.5 9H15z"/></svg>`;
  }

  function showToast(msg, isError) {
    toastEl.hidden = false;
    toastEl.textContent = msg;
    toastEl.classList.toggle('error', !!isError);
    clearTimeout(showToast._t);
    showToast._t = setTimeout(() => {
      toastEl.hidden = true;
    }, 3200);
  }

  function parseSharePath() {
    const path = location.pathname.replace(/\/+$/, '');
    try {
      accessToken = new URLSearchParams(location.search).get('access') || '';
    } catch {
      accessToken = '';
    }
    const short = path.match(/^\/s\/([^/]+)$/);
    if (short) {
      idKind = 'code';
      idValue = decodeURIComponent(short[1]);
      return true;
    }
    const full = path.match(/^\/share\/([^/]+)$/);
    if (full) {
      idKind = 'token';
      idValue = decodeURIComponent(full[1]);
      return true;
    }
    return false;
  }

  function hideAllViews() {
    gateEl.hidden = true;
    loginEl.hidden = true;
    errorEl.hidden = true;
    mainEl.hidden = true;
  }

  function showError(title, message) {
    hideAllViews();
    gateEl.hidden = false;
    errorEl.hidden = false;
    errorTitle.textContent = title;
    errorMsg.textContent = message;
  }

  function showLogin(hintName) {
    hideAllViews();
    gateEl.hidden = false;
    loginEl.hidden = false;
    if (hintName) {
      chipEl.textContent = hintName;
      loginLead.textContent = '“' + hintName + '” requires sign-in to open.';
    }
  }

  function showLoginStatus(msg, isError) {
    loginStatus.hidden = false;
    loginStatus.textContent = msg;
    loginStatus.classList.toggle('error', !!isError);
  }

  async function fetchMeta() {
    const res = await api(withAccess(apiBase()));
    if (res.status === 401) {
      let body = null;
      try {
        body = await res.json();
      } catch {
        body = null;
      }
      showLogin((body && body.display_name) || 'Shared');
      return null;
    }
    if (res.status === 403) {
      showError('Access denied', 'You do not have permission to open this share.');
      return null;
    }
    if (res.status === 410) {
      showError('Link expired', 'This share link has expired or been revoked.');
      return null;
    }
    if (res.status === 404) {
      showError('Not found', 'This share link does not exist.');
      return null;
    }
    if (!res.ok) {
      showError('Error', await res.text());
      return null;
    }
    return res.json();
  }

  function relativeName(fullKey, prefix) {
    if (fullKey.startsWith(prefix)) return fullKey.slice(prefix.length);
    return fullKey;
  }

  function accessLabel(mode) {
    if (mode === 'public') return 'Public link';
    if (mode === 'bucket_readers') return 'Bucket readers';
    return 'Restricted';
  }

  function renderBreadcrumbs() {
    breadcrumbsEl.innerHTML = '';
    const rootName = shareRoot.replace(/\/$/, '').split('/').pop() || 'Shared';
    const under = browsePrefix.startsWith(shareRoot)
      ? browsePrefix.slice(shareRoot.length)
      : '';
    const parts = under.split('/').filter(Boolean);

    const root = document.createElement(parts.length ? 'a' : 'span');
    if (parts.length) {
      root.href = '#';
      root.addEventListener('click', (e) => {
        e.preventDefault();
        browsePrefix = shareRoot;
        loadFolder();
      });
    }
    root.textContent = rootName;
    breadcrumbsEl.appendChild(root);

    let acc = shareRoot;
    parts.forEach((part, idx) => {
      acc += part + '/';
      const sep = document.createElement('span');
      sep.className = 'sep';
      sep.textContent = '/';
      breadcrumbsEl.appendChild(sep);
      const isLast = idx === parts.length - 1;
      const el = document.createElement(isLast ? 'span' : 'a');
      el.textContent = part;
      if (!isLast) {
        const prefix = acc;
        el.href = '#';
        el.addEventListener('click', (e) => {
          e.preventDefault();
          browsePrefix = prefix;
          loadFolder();
        });
      } else {
        el.className = 'current';
      }
      breadcrumbsEl.appendChild(el);
    });
  }

  function openPreview(key) {
    if (typeof window.openPreview === 'function') {
      window.openPreview(key);
      return;
    }
    downloadKey(key);
  }

  async function loadFolder() {
    listingBody.innerHTML = '';
    emptyEl.hidden = true;
    const url = withAccess(
      apiBase() + '/list?prefix=' + encodeURIComponent(browsePrefix),
    );
    const res = await api(url);
    if (res.status === 401) {
      showLogin(meta && meta.display_name);
      return;
    }
    if (!res.ok) {
      showToast(await res.text(), true);
      return;
    }
    const data = await res.json();
    renderBreadcrumbs();

    const folders = data.folders || [];
    const objects = (data.objects || []).filter((o) => {
      const name = o.original_filename.split('/').pop();
      return name !== '.keep';
    });

    for (const folder of folders) {
      const prefix = folder.prefix;
      const name = relativeName(prefix, browsePrefix).replace(/\/$/, '') || prefix;
      const tr = document.createElement('tr');
      tr.innerHTML =
        '<td><div class="name-cell">' +
        folderIconSvg() +
        '<button type="button" class="name-link folder">' +
        esc(name) +
        '</button></div></td>' +
        '<td class="mono">—</td>' +
        '<td class="mono">' +
        esc(formatDate(folder.date_modified)) +
        '</td>' +
        '<td></td>';
      tr.querySelector('.name-link').addEventListener('click', () => {
        browsePrefix = prefix;
        loadFolder();
      });
      listingBody.appendChild(tr);
    }

    for (const o of objects) {
      const key = o.original_filename;
      const name = relativeName(key, browsePrefix) || key;
      const previewable = isPreviewable(key);
      const tr = document.createElement('tr');
      tr.innerHTML =
        '<td><div class="name-cell">' +
        fileIconSvg() +
        '<button type="button" class="name-link file" title="' +
        escAttr(key) +
        '">' +
        esc(name) +
        '</button></div></td>' +
        '<td class="mono">' +
        esc(formatBytes(o.filesize_bytes)) +
        '</td>' +
        '<td class="mono">' +
        esc(formatDate(o.date_modified)) +
        '</td>' +
        '<td><div class="actions">' +
        (previewable
          ? '<button type="button" class="btn ghost" data-preview>Preview</button>'
          : '') +
        '<button type="button" class="btn ghost" data-dl>Download</button>' +
        '</div></td>';
      const open = () => {
        if (previewable) openPreview(key);
        else downloadKey(key);
      };
      tr.querySelector('.name-link').addEventListener('click', open);
      const previewEl = tr.querySelector('[data-preview]');
      if (previewEl) previewEl.addEventListener('click', () => openPreview(key));
      tr.querySelector('[data-dl]').addEventListener('click', () => downloadKey(key));
      listingBody.appendChild(tr);
    }

    emptyEl.hidden = folders.length + objects.length > 0;
  }

  async function downloadKey(key) {
    try {
      const res = await api(contentUrl(key));
      if (res.status === 401) {
        showLogin(meta && meta.display_name);
        return;
      }
      if (!res.ok) throw new Error(await res.text());
      const blob = await res.blob();
      const objectUrl = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = objectUrl;
      a.download = key.split('/').pop() || key;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(objectUrl);
      showToast('Download started');
    } catch (err) {
      showToast(String(err.message || err), true);
    }
  }

  /** @type {AbortController|null} */
  let stageVideoAbort = null;

  function revokeStageUrl() {
    if (stageVideoAbort) {
      stageVideoAbort.abort();
      stageVideoAbort = null;
    }
    if (window.MyS3VideoPreview && typeof window.MyS3VideoPreview.destroy === 'function') {
      window.MyS3VideoPreview.destroy();
    }
    if (stageObjectUrl) {
      URL.revokeObjectURL(stageObjectUrl);
      stageObjectUrl = null;
    }
  }

  function setStageMode(mode) {
    stageLoading.hidden = mode !== 'loading';
    stageBody.hidden = mode !== 'body';
    stageFallback.hidden = mode !== 'fallback';
  }

  async function loadInlineStage(key) {
    revokeStageUrl();
    setStageMode('loading');
    fileStage.hidden = false;
    const ext = fileExt(key);

    try {
      if (IMAGE_EXT.has(ext)) {
        const res = await api(contentUrl(key));
        if (!res.ok) throw new Error(await res.text());
        const blob = await res.blob();
        stageObjectUrl = URL.createObjectURL(blob);
        stageBody.innerHTML =
          '<img class="share-stage-media" alt="' + escAttr(key.split('/').pop() || key) + '" src="' + escAttr(stageObjectUrl) + '" />';
        setStageMode('body');
        return;
      }

      if (VIDEO_EXT.has(ext)) {
        if (
          !window.MyS3VideoPreview ||
          typeof window.MyS3VideoPreview.play !== 'function' ||
          !window.MyS3VideoPreview.isSupported()
        ) {
          throw new Error('This browser cannot preview video.');
        }
        const native =
          typeof window.MyS3VideoPreview.isNativePlayable === 'function' &&
          window.MyS3VideoPreview.isNativePlayable(key);
        if (
          !native &&
          typeof window.MyS3VideoPreview.canTranscode === 'function' &&
          !window.MyS3VideoPreview.canTranscode()
        ) {
          throw new Error(
            'This browser cannot preview this format (needs MediaSource and Origin Private File System).'
          );
        }
        const controller = new AbortController();
        stageVideoAbort = controller;
        stageBody.innerHTML =
          '<div class="preview-media-video share-stage-video">' +
          '<div class="preview-loading" id="share-stage-video-status">Loading…</div>' +
          '<video class="share-stage-media" id="share-stage-video-el" controls playsinline></video>' +
          '</div>';
        setStageMode('body');
        const videoEl = stageBody.querySelector('#share-stage-video-el');
        const statusEl = stageBody.querySelector('#share-stage-video-status');
        const setStatus = (msg) => {
          if (!statusEl) return;
          if (!msg) {
            statusEl.hidden = true;
            statusEl.textContent = '';
            return;
          }
          statusEl.hidden = false;
          statusEl.textContent = msg;
        };
        await window.MyS3VideoPreview.play({
          video: videoEl,
          key,
          nativeUrl: native ? contentUrl(key) : null,
          height: native ? 'original' : '720',
          signal: controller.signal,
          onStatus: setStatus,
          fetchContent: () => api(contentUrl(key), { signal: controller.signal }),
        });
        setStatus('');
        // Keep stageVideoAbort until revokeStageUrl so in-flight encodes can be cancelled.
        return;
      }

      if (TEXT_EXT.has(ext)) {
        const res = await api(contentUrl(key));
        if (!res.ok) throw new Error(await res.text());
        const len = Number(res.headers.get('content-length') || 0);
        if (len > 512 * 1024) {
          setStageMode('fallback');
          stageFallback.querySelector('p').textContent =
            'This text file is large — open preview or download it.';
          stageDownload.textContent = 'Open preview';
          stageDownload.onclick = () => openPreview(key);
          return;
        }
        const text = await res.text();
        const snippet = text.length > 12000 ? text.slice(0, 12000) + '\n…' : text;
        stageBody.innerHTML =
          '<pre class="share-stage-text">' + esc(snippet) + '</pre>';
        setStageMode('body');
        return;
      }

      if (isPreviewable(key)) {
        setStageMode('fallback');
        stageFallback.querySelector('p').textContent = 'Preview is available for this file.';
        stageDownload.textContent = 'Open preview';
        stageDownload.onclick = () => openPreview(key);
        return;
      }

      setStageMode('fallback');
      stageFallback.querySelector('p').textContent = 'This file can’t be previewed here.';
      stageDownload.textContent = 'Download';
      stageDownload.onclick = () => downloadKey(key);
    } catch (err) {
      if (err && err.name === 'AbortError') return;
      setStageMode('fallback');
      stageFallback.querySelector('p').textContent = String(err.message || err);
      stageDownload.textContent = 'Download';
      stageDownload.onclick = () => downloadKey(key);
    }
  }

  async function renderShare() {
    meta = await fetchMeta();
    if (!meta) return;

    hideAllViews();
    mainEl.hidden = false;
    titleEl.textContent = meta.display_name || 'Shared item';
    document.title = (meta.display_name || 'Shared') + ' — MyS3';
    chipEl.textContent = meta.bucket_name || 'Shared';

    const bits = [accessLabel(meta.access_mode)];
    if (meta.filesize_bytes != null) bits.push(formatBytes(meta.filesize_bytes));
    if (meta.expires_at) bits.push('Expires ' + formatDate(meta.expires_at));
    else bits.push('No expiry');
    metaEl.textContent = bits.join(' · ');

    if (meta.target_kind === 'file') {
      kickerEl.textContent = 'Shared file';
      heroIcon.innerHTML = fileIconSvg();
      folderView.hidden = true;
      fileActions.hidden = false;
      previewBtn.hidden = !isPreviewable(meta.target_key);
      previewBtn.onclick = () => openPreview(meta.target_key);
      downloadBtn.onclick = () => downloadKey(meta.target_key);
      stageDownload.onclick = () => downloadKey(meta.target_key);
      await loadInlineStage(meta.target_key);
    } else {
      kickerEl.textContent = 'Shared folder';
      heroIcon.innerHTML = folderIconSvg();
      fileActions.hidden = true;
      fileStage.hidden = true;
      folderView.hidden = false;
      shareRoot = meta.target_key;
      browsePrefix = shareRoot;
      await loadFolder();
    }
  }

  // Hooks for preview.js (loaded after this script).
  window.MyS3 = {
    api,
    contentUrl,
    downloadObject: downloadKey,
    formatBytes,
    showStatus: showToast,
    authHeaders,
    getToken,
  };

  document.getElementById('share-login-btn').addEventListener('click', async () => {
    const username_hex = document.getElementById('share-login-user').value.trim();
    const password_hex = document.getElementById('share-login-pass').value.trim();
    if (!username_hex || !password_hex) {
      showLoginStatus('Enter username and password.', true);
      return;
    }
    try {
      const res = await fetch('/api/v1/auth/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username_hex, password_hex }),
      });
      if (!res.ok) throw new Error(await res.text());
      const data = await res.json();
      if (data.session_token) setToken(data.session_token);
      showLoginStatus('Signed in…');
      await renderShare();
    } catch (err) {
      showLoginStatus(String(err.message || err), true);
    }
  });

  document.getElementById('share-login-pass').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') document.getElementById('share-login-btn').click();
  });

  async function boot() {
    if (!parseSharePath()) {
      showError('Invalid link', 'This URL is not a valid share link.');
      return;
    }
    await renderShare();
  }

  boot();
})();
