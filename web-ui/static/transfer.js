(function () {
  /** @typedef {'queued'|'active'|'done'|'error'|'cancelled'} JobStatus */
  /** @typedef {{ id: number, kind: 'upload'|'download', key: string, label: string, totalBytes: number, loadedBytes: number, status: JobStatus, error?: string, file?: File, xhr?: XMLHttpRequest }} TransferJob */

  /** @type {TransferJob[]} */
  let jobs = [];
  let nextId = 1;
  let processing = false;
  let cancelAll = false;
  let hideTimer = null;
  /** @type {{ t: number, loaded: number }[]} */
  let speedSamples = [];
  let expanded = false;

  const dock = document.getElementById('transfer-dock');
  if (!dock) return;

  const summaryEl = document.getElementById('transfer-summary');
  const etaEl = document.getElementById('transfer-eta');
  const countEl = document.getElementById('transfer-count');
  const barEl = document.getElementById('transfer-bar');
  const listEl = document.getElementById('transfer-list');
  const expandBtn = document.getElementById('transfer-expand');
  const cancelAllBtn = document.getElementById('transfer-cancel-all');
  const dismissBtn = document.getElementById('transfer-dismiss');

  function authHeaders(extra) {
    return window.MyS3 ? window.MyS3.authHeaders(extra) : Object.assign({}, extra || {});
  }

  function encodeKeyPath(key) {
    return window.MyS3 ? window.MyS3.encodeKeyPath(key) : encodeURIComponent(key);
  }

  function bucket() {
    return window.MyS3 ? window.MyS3.getBucket() : 'storage';
  }

  function basename(key) {
    const parts = String(key).split('/');
    return parts[parts.length - 1] || key;
  }

  function formatBytes(n) {
    if (window.MyS3 && window.MyS3.formatBytes) return window.MyS3.formatBytes(n);
    if (n < 1024) return n + ' B';
    if (n < 1024 ** 2) return (n / 1024).toFixed(1) + ' KiB';
    if (n < 1024 ** 3) return (n / 1024 ** 2).toFixed(1) + ' MiB';
    return (n / 1024 ** 3).toFixed(2) + ' GiB';
  }

  function formatEta(seconds) {
    if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return '—';
    if (seconds < 1) return '<1s';
    if (seconds < 60) return Math.ceil(seconds) + 's';
    const m = Math.floor(seconds / 60);
    const s = Math.ceil(seconds % 60);
    if (m < 60) return m + 'm ' + s + 's';
    const h = Math.floor(m / 60);
    return h + 'h ' + (m % 60) + 'm';
  }

  function pendingJobs() {
    return jobs.filter((j) => j.status === 'queued' || j.status === 'active');
  }

  function batchJobs() {
    // Current visible batch: unfinished + recently finished in this session list
    return jobs;
  }

  function showDock() {
    clearTimeout(hideTimer);
    hideTimer = null;
    dock.hidden = false;
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (pendingJobs().length) return;
      const hasError = jobs.some((j) => j.status === 'error');
      if (hasError) return;
      jobs = [];
      speedSamples = [];
      dock.hidden = true;
      listEl.hidden = true;
      expanded = false;
      expandBtn.setAttribute('aria-expanded', 'false');
      expandBtn.textContent = 'Details';
      render();
    }, 2800);
  }

  function pushSpeedSample(loaded) {
    const t = Date.now();
    speedSamples.push({ t, loaded });
    while (speedSamples.length > 24) speedSamples.shift();
    while (speedSamples.length > 2 && t - speedSamples[0].t > 8000) speedSamples.shift();
  }

  function estimateEtaSeconds() {
    const active = pendingJobs();
    if (!active.length) return null;
    let loaded = 0;
    let total = 0;
    for (const j of jobs) {
      if (j.status === 'cancelled') continue;
      loaded += j.loadedBytes;
      total += j.totalBytes > 0 ? j.totalBytes : j.loadedBytes;
    }
    const remaining = Math.max(0, total - loaded);
    if (remaining === 0) return 0;
    if (speedSamples.length < 2) return null;
    const first = speedSamples[0];
    const last = speedSamples[speedSamples.length - 1];
    const dt = (last.t - first.t) / 1000;
    const dBytes = last.loaded - first.loaded;
    if (dt <= 0 || dBytes <= 0) return null;
    const bps = dBytes / dt;
    return remaining / bps;
  }

  function render() {
    const all = batchJobs();
    const uploads = all.filter((j) => j.kind === 'upload');
    const downloads = all.filter((j) => j.kind === 'download');
    const pending = pendingJobs();
    const doneCount = all.filter((j) => j.status === 'done').length;
    const totalCount = all.filter((j) => j.status !== 'cancelled').length || all.length;
    const finishedLike = all.filter(
      (j) => j.status === 'done' || j.status === 'error' || j.status === 'cancelled',
    ).length;

    let loaded = 0;
    let total = 0;
    for (const j of all) {
      if (j.status === 'cancelled') continue;
      loaded += j.loadedBytes;
      total += j.totalBytes > 0 ? j.totalBytes : j.status === 'done' ? j.loadedBytes : 0;
      if (j.status === 'active' || j.status === 'queued') {
        if (j.totalBytes <= 0) total += Math.max(j.loadedBytes, 1);
      }
    }
    const pct = total > 0 ? Math.min(100, Math.round((loaded / total) * 100)) : pending.length ? 0 : 100;

    let kindLabel = 'Transfers';
    if (uploads.length && !downloads.length) kindLabel = 'Uploading';
    else if (downloads.length && !uploads.length) kindLabel = 'Downloading';
    else if (uploads.length && downloads.length) kindLabel = 'Transferring';

    const completed = Math.min(doneCount, totalCount);
    summaryEl.textContent = pending.length
      ? `${kindLabel}`
      : all.some((j) => j.status === 'error')
        ? 'Transfers finished with errors'
        : 'Transfers complete';
    countEl.textContent = `${completed}/${totalCount || all.length} objects`;
    etaEl.textContent = pending.length
      ? `ETA ${formatEta(estimateEtaSeconds())}`
      : all.some((j) => j.status === 'error')
        ? 'Needs attention'
        : 'Done';
    barEl.style.width = pct + '%';
    barEl.parentElement.setAttribute('aria-valuenow', String(pct));

    cancelAllBtn.disabled = !pending.length;
    dismissBtn.hidden = !!pending.length;

    listEl.innerHTML = '';
    for (const j of all) {
      const row = document.createElement('div');
      row.className = 'transfer-job' + (j.status === 'error' ? ' error' : '');
      const pctJob =
        j.totalBytes > 0
          ? Math.min(100, Math.round((j.loadedBytes / j.totalBytes) * 100))
          : j.status === 'done'
            ? 100
            : 0;
      const statusText =
        j.status === 'error'
          ? j.error || 'Failed'
          : j.status === 'cancelled'
            ? 'Cancelled'
            : j.status === 'done'
              ? 'Done'
              : j.status === 'active'
                ? pctJob + '%'
                : 'Queued';
      row.innerHTML = `
        <div class="transfer-job-meta">
          <span class="transfer-job-kind">${j.kind === 'upload' ? '↑' : '↓'}</span>
          <span class="transfer-job-name" title="${escAttr(j.key)}">${esc(j.label)}</span>
          <span class="transfer-job-status">${esc(statusText)}</span>
        </div>
        <div class="transfer-job-bar"><i style="width:${pctJob}%"></i></div>
        <div class="transfer-job-actions"></div>`;
      const actions = row.querySelector('.transfer-job-actions');
      if (j.status === 'queued' || j.status === 'active') {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'btn ghost transfer-job-cancel';
        btn.textContent = 'Cancel';
        btn.addEventListener('click', () => cancelJob(j.id));
        actions.appendChild(btn);
      }
      listEl.appendChild(row);
    }

    listEl.hidden = !expanded;
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

  function enqueue(job) {
    jobs.push(job);
    showDock();
    render();
    pump();
  }

  function uploadLabel(key) {
    if (key.endsWith('.keep')) {
      const dir = key.slice(0, -'.keep'.length).replace(/\/$/, '');
      const name = basename(dir);
      return (name || dir || 'folder') + '/';
    }
    return basename(key);
  }

  function enqueueUpload(file, key) {
    enqueue({
      id: nextId++,
      kind: 'upload',
      key,
      label: uploadLabel(key),
      totalBytes: file.size || 0,
      loadedBytes: 0,
      status: 'queued',
      file,
    });
  }

  function enqueueDownload(key, sizeHint) {
    enqueue({
      id: nextId++,
      kind: 'download',
      key,
      label: basename(key),
      totalBytes: sizeHint > 0 ? sizeHint : 0,
      loadedBytes: 0,
      status: 'queued',
    });
  }

  function relativeUploadPath(file) {
    const rel = (file && (file.webkitRelativePath || file.name)) || '';
    return String(rel).replace(/\\/g, '/').replace(/^\/+/, '');
  }

  function enqueueUploads(fileList, prefix) {
    const files = Array.from(fileList || []);
    const p = prefix || '';
    const dirPrefixes = new Set();

    for (const file of files) {
      const rel = relativeUploadPath(file);
      if (!rel || rel.endsWith('/')) continue;
      const parts = rel.split('/');
      if (parts.length > 1) {
        let acc = '';
        for (let i = 0; i < parts.length - 1; i++) {
          if (!parts[i] || parts[i] === '.' || parts[i] === '..') continue;
          acc += parts[i] + '/';
          dirPrefixes.add(acc);
        }
      }
    }

    // Folder markers first so prefixes exist even if later file uploads are cancelled.
    for (const dir of Array.from(dirPrefixes).sort()) {
      const keepFile = new File([], '.keep', { type: 'application/octet-stream' });
      enqueueUpload(keepFile, p + dir + '.keep');
    }

    for (const file of files) {
      const rel = relativeUploadPath(file);
      if (!rel || rel.endsWith('/')) continue;
      const parts = rel.split('/');
      if (parts.some((seg) => seg === '.' || seg === '..')) continue;
      enqueueUpload(file, p + rel);
    }
  }

  function enqueueDownloads(items) {
    for (const item of items || []) {
      if (typeof item === 'string') enqueueDownload(item, 0);
      else enqueueDownload(item.key, item.size || 0);
    }
  }

  function cancelJob(id) {
    const job = jobs.find((j) => j.id === id);
    if (!job) return;
    if (job.status === 'active' && job.xhr) {
      job.xhr.abort();
    } else if (job.status === 'queued') {
      job.status = 'cancelled';
      render();
      if (!pendingJobs().length) scheduleHide();
    }
  }

  function cancelAllJobs() {
    cancelAll = true;
    for (const j of jobs) {
      if (j.status === 'queued') j.status = 'cancelled';
      if (j.status === 'active' && j.xhr) j.xhr.abort();
    }
    render();
  }

  function runUpload(job) {
    return new Promise((resolve) => {
      const xhr = new XMLHttpRequest();
      job.xhr = xhr;
      job.status = 'active';
      render();
      const url =
        '/api/v1/objects/' +
        encodeKeyPath(job.key) +
        '?bucket=' +
        encodeURIComponent(bucket());
      xhr.open('PUT', url);
      const headers = authHeaders({
        'Content-Type': (job.file && job.file.type) || 'application/octet-stream',
      });
      Object.keys(headers).forEach((k) => xhr.setRequestHeader(k, headers[k]));
      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) {
          job.totalBytes = e.total;
          job.loadedBytes = e.loaded;
        } else {
          job.loadedBytes = e.loaded;
        }
        pushSpeedSample(jobs.reduce((s, j) => s + j.loadedBytes, 0));
        render();
      };
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) {
          job.status = 'done';
          job.loadedBytes = job.totalBytes || job.loadedBytes;
        } else {
          job.status = 'error';
          job.error = xhr.responseText || 'HTTP ' + xhr.status;
        }
        job.xhr = undefined;
        resolve();
      };
      xhr.onerror = () => {
        job.status = 'error';
        job.error = 'Network error';
        job.xhr = undefined;
        resolve();
      };
      xhr.onabort = () => {
        job.status = 'cancelled';
        job.xhr = undefined;
        resolve();
      };
      xhr.send(job.file);
    });
  }

  function runDownload(job) {
    return new Promise((resolve) => {
      const xhr = new XMLHttpRequest();
      job.xhr = xhr;
      job.status = 'active';
      render();
      const url =
        '/api/v1/objects/content/' +
        encodeKeyPath(job.key) +
        '?bucket=' +
        encodeURIComponent(bucket());
      xhr.open('GET', url);
      xhr.responseType = 'blob';
      const headers = authHeaders();
      Object.keys(headers).forEach((k) => xhr.setRequestHeader(k, headers[k]));
      xhr.onprogress = (e) => {
        if (e.lengthComputable) {
          job.totalBytes = e.total;
          job.loadedBytes = e.loaded;
        } else {
          job.loadedBytes = e.loaded;
        }
        pushSpeedSample(jobs.reduce((s, j) => s + j.loadedBytes, 0));
        render();
      };
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) {
          job.status = 'done';
          if (job.totalBytes <= 0 && xhr.response) {
            job.totalBytes = xhr.response.size || job.loadedBytes;
            job.loadedBytes = job.totalBytes;
          }
          try {
            const blobUrl = URL.createObjectURL(xhr.response);
            const link = document.createElement('a');
            link.href = blobUrl;
            link.download = basename(job.key);
            document.body.appendChild(link);
            link.click();
            link.remove();
            setTimeout(() => URL.revokeObjectURL(blobUrl), 2000);
          } catch (err) {
            job.status = 'error';
            job.error = String(err.message || err);
          }
        } else {
          job.status = 'error';
          job.error = 'HTTP ' + xhr.status;
        }
        job.xhr = undefined;
        resolve();
      };
      xhr.onerror = () => {
        job.status = 'error';
        job.error = 'Network error';
        job.xhr = undefined;
        resolve();
      };
      xhr.onabort = () => {
        job.status = 'cancelled';
        job.xhr = undefined;
        resolve();
      };
      xhr.send();
    });
  }

  async function pump() {
    if (processing) return;
    processing = true;
    cancelAll = false;
    speedSamples = [];
    try {
      while (true) {
        const next = jobs.find((j) => j.status === 'queued');
        if (!next) break;
        if (cancelAll) {
          next.status = 'cancelled';
          continue;
        }
        if (next.kind === 'upload') await runUpload(next);
        else await runDownload(next);
        render();
      }
      const hadUploads = jobs.some((j) => j.kind === 'upload' && j.status === 'done');
      if (hadUploads && window.MyS3 && typeof window.MyS3.refresh === 'function') {
        try {
          await window.MyS3.refresh();
        } catch {
          /* ignore */
        }
      }
      if (!pendingJobs().length) scheduleHide();
    } finally {
      processing = false;
      render();
    }
  }

  expandBtn.addEventListener('click', () => {
    expanded = !expanded;
    expandBtn.setAttribute('aria-expanded', expanded ? 'true' : 'false');
    expandBtn.textContent = expanded ? 'Hide' : 'Details';
    render();
  });

  cancelAllBtn.addEventListener('click', () => cancelAllJobs());

  dismissBtn.addEventListener('click', () => {
    jobs = [];
    speedSamples = [];
    dock.hidden = true;
    expanded = false;
    listEl.hidden = true;
    expandBtn.setAttribute('aria-expanded', 'false');
    expandBtn.textContent = 'Details';
    render();
  });

  window.MyS3Transfers = {
    enqueueUpload,
    enqueueDownload,
    enqueueUploads,
    enqueueDownloads,
    cancelAll: cancelAllJobs,
  };
})();
