(function () {
  const statusEl = document.getElementById('actions-status');
  const transfersBody = document.getElementById('actions-transfers-body');
  const transfersEmpty = document.getElementById('actions-transfers-empty');
  const etagList = document.getElementById('actions-etag-list');
  const etagEmpty = document.getElementById('actions-etag-empty');
  const filterBar = document.getElementById('actions-transfer-filter');
  const PAGE_SIZE = (window.MyS3Pager && window.MyS3Pager.PAGE_SIZE) || 25;

  /** @type {'all'|'upload'|'download'} */
  let transferFilter = 'all';
  let transfersPage = 1;
  let etagPage = 1;
  /** @type {object[]} */
  let etagBucketsCache = [];
  /** @type {ReturnType<typeof setInterval>|null} */
  let etagPollTimer = null;
  /** @type {(() => void)|null} */
  let unsubTransfers = null;
  /** @type {object[]} */
  let liveJobs = [];
  /** @type {object[]} */
  let history = [];

  const ETAG_OPTIONS = [
    ['md5', 'MD5'],
    ['sha256', 'SHA-256'],
    ['sha512', 'SHA-512'],
    ['blake2-128', 'Blake2-128'],
    ['blake2-256', 'Blake2-256'],
    ['blake3-128', 'Blake3-128'],
    ['blake3-256', 'Blake3-256'],
  ];

  function api() {
    return window.MyS3.api;
  }

  function showStatus(msg, isError) {
    if (!statusEl) return;
    if (!msg) {
      statusEl.hidden = true;
      statusEl.textContent = '';
      return;
    }
    statusEl.hidden = false;
    statusEl.textContent = msg;
    statusEl.classList.toggle('error', !!isError);
  }

  function formatBytes(n) {
    return window.MyS3.formatBytes(n);
  }

  function formatWhen(ts) {
    if (!ts) return '—';
    try {
      return new Date(ts).toLocaleString();
    } catch {
      return '—';
    }
  }

  function statusLabel(status, error) {
    if (status === 'error') return error || 'Failed';
    if (status === 'cancelled') return 'Cancelled';
    if (status === 'done') return 'Done';
    if (status === 'active') return 'Active';
    if (status === 'queued') return 'Queued';
    return status || '—';
  }

  function mergedTransferRows() {
    const live = (liveJobs || []).map((j) => ({
      id: 'live-' + j.id,
      kind: j.kind,
      key: j.key,
      label: j.label,
      bucket: j.bucket || window.MyS3.getBucket(),
      totalBytes: j.totalBytes || j.loadedBytes || 0,
      loadedBytes: j.loadedBytes || 0,
      status: j.status,
      error: j.error,
      when: j.finishedAt || j.startedAt || Date.now(),
      live: true,
    }));
    const hist = (history || []).map((h) => ({
      id: h.id,
      kind: h.kind,
      key: h.key,
      label: h.label,
      bucket: h.bucket,
      totalBytes: h.totalBytes || 0,
      loadedBytes: h.totalBytes || 0,
      status: h.status,
      error: h.error,
      when: h.finishedAt,
      live: false,
    }));
    const histFiltered = hist.filter((h) => h.status !== 'queued' && h.status !== 'active');
    const rows = live.concat(histFiltered);
    rows.sort((a, b) => (b.when || 0) - (a.when || 0));
    return rows.filter((r) => {
      if (transferFilter === 'all') return true;
      return r.kind === transferFilter;
    });
  }

  function renderTransfers(page) {
    if (!transfersBody) return;
    const allRows = mergedTransferRows();
    const sliced = window.MyS3Pager
      ? window.MyS3Pager.slice(allRows, page == null ? transfersPage : page, PAGE_SIZE)
      : { items: allRows, page: 1, total: allRows.length };
    transfersPage = sliced.page;
    transfersBody.innerHTML = '';
    if (transfersEmpty) transfersEmpty.hidden = sliced.total > 0;
    for (const r of sliced.items) {
      const tr = document.createElement('tr');
      const pct =
        r.live && r.totalBytes > 0
          ? Math.min(100, Math.round((r.loadedBytes / r.totalBytes) * 100))
          : r.status === 'done'
            ? 100
            : null;
      const sizeText =
        pct != null && r.status !== 'done' && r.status !== 'error' && r.status !== 'cancelled'
          ? `${formatBytes(r.loadedBytes)} (${pct}%)`
          : formatBytes(r.totalBytes);
      tr.innerHTML = `
        <td>${r.kind === 'upload' ? 'Upload' : 'Download'}</td>
        <td class="mono" title="${window.MyS3.escAttr(r.key || '')}">${window.MyS3.esc(r.label || r.key || '')}</td>
        <td class="mono">${window.MyS3.esc(r.bucket || '')}</td>
        <td class="${r.status === 'error' ? 'error-text' : ''}">${window.MyS3.esc(statusLabel(r.status, r.error))}</td>
        <td class="mono">${window.MyS3.esc(sizeText)}</td>
        <td class="mono">${window.MyS3.esc(formatWhen(r.when))}</td>`;
      transfersBody.appendChild(tr);
    }
    if (window.MyS3Pager) {
      window.MyS3Pager.render(document.getElementById('actions-transfers-pager'), {
        page: transfersPage,
        total: sliced.total,
        pageSize: PAGE_SIZE,
        onChange: (p) => renderTransfers(p),
      });
    }
  }

  function stopEtagPoll() {
    if (etagPollTimer != null) {
      clearInterval(etagPollTimer);
      etagPollTimer = null;
    }
  }

  function rehashStatusLabel(b) {
    const s = b.etag_rehash_status;
    if (s === 'running') {
      return `Rehashing ${b.etag_rehash_processed || 0} / ${b.etag_rehash_total || 0}`;
    }
    if (s === 'done') {
      return `Done (${b.etag_rehash_processed || 0} / ${b.etag_rehash_total || 0})`;
    }
    if (s === 'error') {
      return b.etag_rehash_error ? `Error: ${b.etag_rehash_error}` : 'Error';
    }
    return 'Idle';
  }

  function renderEtagBuckets(page) {
    if (!etagList) return;
    const editable = (etagBucketsCache || []).filter((b) => b.can_edit_replication);
    const sliced = window.MyS3Pager
      ? window.MyS3Pager.slice(editable, page == null ? etagPage : page, PAGE_SIZE)
      : { items: editable, page: 1, total: editable.length };
    etagPage = sliced.page;
    etagList.innerHTML = '';
    if (etagEmpty) etagEmpty.hidden = sliced.total > 0;
    let anyRunning = editable.some((b) => b.etag_rehash_status === 'running');
    for (const b of sliced.items) {
      const card = document.createElement('article');
      card.className = 'actions-etag-card';
      card.dataset.bucketId = String(b.id);
      const opts = ETAG_OPTIONS.map(
        ([v, label]) =>
          `<option value="${v}" ${b.etag_type === v ? 'selected' : ''}>${label}</option>`,
      ).join('');
      const running = b.etag_rehash_status === 'running';
      card.innerHTML = `
        <div class="actions-etag-head">
          <strong class="mono">${window.MyS3.esc(b.name)}</strong>
          <span class="actions-etag-status ${b.etag_rehash_status === 'error' ? 'error' : ''} ${
            running ? 'running' : ''
          }">${window.MyS3.esc(rehashStatusLabel(b))}</span>
        </div>
        <div class="actions-etag-form">
          <label class="field">
            Algorithm
            <select class="actions-etag-type" ${running ? 'disabled' : ''}>${opts}</select>
          </label>
          <div class="field">
            <span class="field-label">Apply to</span>
            <div class="nodes-mode-toggle actions-etag-apply" role="group" aria-label="Apply mode">
              <button type="button" class="nodes-mode-opt is-active" data-apply="new_only" ${
                running ? 'disabled' : ''
              }>New objects only</button>
              <button type="button" class="nodes-mode-opt" data-apply="recalculate_all" ${
                running ? 'disabled' : ''
              }>Recalculate all</button>
            </div>
            <input type="hidden" class="actions-etag-apply-val" value="new_only" />
          </div>
          <div class="actions-etag-actions">
            <button type="button" class="btn primary actions-etag-apply-btn" ${
              running ? 'disabled' : ''
            }>Apply</button>
          </div>
        </div>`;
      card.querySelectorAll('.actions-etag-apply .nodes-mode-opt').forEach((btn) => {
        btn.addEventListener('click', () => {
          if (btn.disabled) return;
          card.querySelector('.actions-etag-apply-val').value = btn.dataset.apply;
          card.querySelectorAll('.actions-etag-apply .nodes-mode-opt').forEach((b2) => {
            b2.classList.toggle('is-active', b2.dataset.apply === btn.dataset.apply);
          });
        });
      });
      card.querySelector('.actions-etag-apply-btn').addEventListener('click', () => {
        applyEtag(b, card).catch((err) => showStatus(String(err.message || err), true));
      });
      etagList.appendChild(card);
    }
    if (window.MyS3Pager) {
      window.MyS3Pager.render(document.getElementById('actions-etag-pager'), {
        page: etagPage,
        total: sliced.total,
        pageSize: PAGE_SIZE,
        onChange: (p) => renderEtagBuckets(p),
      });
    }
    if (anyRunning && etagPollTimer == null) {
      etagPollTimer = setInterval(() => {
        loadEtagBuckets().catch(() => {});
      }, 2000);
    }
    if (!anyRunning) stopEtagPoll();
  }

  async function applyEtag(bucket, card) {
    const etag_type = card.querySelector('.actions-etag-type').value;
    const apply = card.querySelector('.actions-etag-apply-val').value || 'new_only';
    if (apply === 'recalculate_all') {
      const ok = await window.MyS3UI.confirm(
        `Recalculate ETags for every object in “${bucket.name}”?`,
        'Recalculate ETags',
      );
      if (!ok) return;
    }
    const res = await api()(`/api/v1/buckets/${bucket.id}/etag`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ etag_type, apply }),
    });
    if (!res.ok && res.status !== 204 && res.status !== 202) {
      throw new Error(await res.text());
    }
    showStatus(
      apply === 'recalculate_all'
        ? `Recalculating ${bucket.name} with ${etag_type}…`
        : `ETag algorithm for ${bucket.name} set to ${etag_type}`,
    );
    await loadEtagBuckets();
  }

  async function loadEtagBuckets() {
    const res = await api()('/api/v1/buckets');
    if (!res.ok) throw new Error(await res.text());
    etagBucketsCache = await res.json();
    renderEtagBuckets(etagPage);
  }

  function bindTransferFilter() {
    if (!filterBar || filterBar.dataset.bound) return;
    filterBar.dataset.bound = '1';
    filterBar.querySelectorAll('[data-transfer-filter]').forEach((btn) => {
      btn.addEventListener('click', () => {
        transferFilter = btn.dataset.transferFilter || 'all';
        filterBar.querySelectorAll('[data-transfer-filter]').forEach((b) => {
          b.classList.toggle('active', b.dataset.transferFilter === transferFilter);
        });
        renderTransfers(1);
      });
    });
    const clearBtn = document.getElementById('actions-clear-history');
    if (clearBtn) {
      clearBtn.addEventListener('click', async () => {
        const ok = await window.MyS3UI.confirm(
          'Clear saved transfer history on this device?',
          'Clear history',
        );
        if (!ok) return;
        if (window.MyS3Transfers) window.MyS3Transfers.clearHistory();
        history = [];
        renderTransfers(1);
        showStatus('Transfer history cleared');
      });
    }
  }

  function ensureTransferSub() {
    if (unsubTransfers || !window.MyS3Transfers) return;
    unsubTransfers = window.MyS3Transfers.subscribe((payload) => {
      liveJobs = payload.jobs || [];
      history = payload.history || [];
      renderTransfers();
    });
  }

  async function show() {
    showStatus('');
    bindTransferFilter();
    ensureTransferSub();
    if (window.MyS3Transfers) {
      liveJobs = window.MyS3Transfers.getJobs() || [];
      history = window.MyS3Transfers.getHistory() || [];
    }
    renderTransfers();
    try {
      await loadEtagBuckets();
    } catch (err) {
      showStatus(String(err.message || err), true);
    }
  }

  function hide() {
    stopEtagPoll();
  }

  window.MyS3Actions = {
    show,
    hide,
    onAuth() {
      /* no-op; permissions come from bucket list */
    },
  };
})();
