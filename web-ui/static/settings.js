(function () {
  const settingsStatus = document.getElementById('settings-status');
  const PAGE_SIZE = (window.MyS3Pager && window.MyS3Pager.PAGE_SIZE) || 25;
  const panels = {
    accounts: document.getElementById('panel-accounts'),
    roles: document.getElementById('panel-roles'),
    buckets: document.getElementById('panel-buckets'),
    storage: document.getElementById('panel-storage'),
    recycle: document.getElementById('panel-recycle'),
  };
  let accountsPage = 1;
  let rolesPage = 1;
  let bucketsPage = 1;
  let recyclePage = 1;
  /** @type {object[]} */
  let accountsCache = [];
  /** @type {object[]} */
  let recycleCache = [];
  /** @type {object[]} */
  let directoryCache = [];

  /** @type {object|null} */
  let storageInfo = null;
  /** @type {string} */
  let nodesLocalStoragePath = '';
  const navLinks = document.querySelectorAll('.settings-nav a');

  let rolesCache = [];
  let bucketsCache = [];
  let selectedRoleId = null;

  function api() {
    return window.MyS3.api;
  }

  function showSettingsStatus(msg, isError) {
    settingsStatus.hidden = false;
    settingsStatus.textContent = msg;
    settingsStatus.classList.toggle('error', !!isError);
    clearTimeout(showSettingsStatus._t);
    showSettingsStatus._t = setTimeout(() => {
      settingsStatus.hidden = true;
    }, 4000);
  }

  function showPanel(name) {
    const key = panels[name] ? name : 'accounts';
    Object.entries(panels).forEach(([k, el]) => {
      el.hidden = k !== key;
    });
    navLinks.forEach((a) => {
      a.classList.toggle('active', a.dataset.panel === key);
    });
    loadPanel(key);
  }

  async function loadPanel(key) {
    try {
      if (key === 'accounts') await loadAccounts();
      if (key === 'roles') await loadRoles();
      if (key === 'buckets') await loadBuckets();
      if (key === 'storage') await loadStorage();
      if (key === 'recycle') await loadRecycle();
    } catch (err) {
      showSettingsStatus(String(err.message || err), true);
    }
  }

  async function loadStorage() {
    const res = await api()('/api/v1/settings/storage');
    if (!res.ok) throw new Error(await res.text());
    storageInfo = await res.json();
    document.getElementById('storage-node-id').textContent = storageInfo.node_id || '—';
    document.getElementById('storage-current-path').textContent =
      storageInfo.absolute_path || storageInfo.path || '—';
    const dbCount = storageInfo.db_object_count != null
      ? storageInfo.db_object_count
      : storageInfo.object_count || 0;
    const diskCount = storageInfo.disk_file_count != null ? storageInfo.disk_file_count : '—';
    const usage = `${window.MyS3.formatBytes(storageInfo.used_bytes || 0)} · ${dbCount} active object${
      dbCount === 1 ? '' : 's'
    }`;
    document.getElementById('storage-usage').textContent = usage;
    document.getElementById('storage-db-count').textContent =
      `${dbCount} active (incl. .keep)`;
    document.getElementById('storage-disk-count').textContent =
      typeof diskCount === 'number' ? `${diskCount} file${diskCount === 1 ? '' : 's'}` : '—';
    const input = document.getElementById('storage-path-input');
    if (input && !input.dataset.touched) {
      input.value = storageInfo.absolute_path || storageInfo.path || '';
    }
    document.getElementById('storage-restart-banner').hidden = true;
  }

  function renderIntegrityReport(report) {
    const el = document.getElementById('storage-integrity-report');
    if (!el || !report) return;
    el.hidden = false;
    const mismatch =
      (report.orphans_removed_count || 0) > 0 ||
      (report.missing_active_count || 0) > 0 ||
      (report.missing_recycle_count || 0) > 0;
    const headline = report.repaired
      ? mismatch
        ? 'Reconcile finished — review remaining issues'
        : 'Reconcile finished — storage looks consistent'
      : mismatch
        ? 'Issues found'
        : 'Looks consistent';
    const listHtml = (title, items, count, cls) => {
      if (!count) return '';
      const shown = (items || []).map((p) => `<li>${window.MyS3.esc(p)}</li>`).join('');
      const more =
        count > (items || []).length
          ? `<li>…and ${count - (items || []).length} more</li>`
          : '';
      return `<div class="${cls}"><strong>${title} (${count})</strong><ul class="integrity-list">${shown}${more}</ul></div>`;
    };
    el.innerHTML = `
      <p class="${mismatch ? 'integrity-warn' : 'integrity-ok'}"><strong>${window.MyS3.esc(headline)}</strong></p>
      <div class="integrity-grid">
        <div class="integrity-stat"><span>DB rows</span><strong>${report.db_rows ?? 0}</strong></div>
        <div class="integrity-stat"><span>Active</span><strong>${report.db_active ?? 0}</strong></div>
        <div class="integrity-stat"><span>Recycled</span><strong>${report.db_recycled ?? 0}</strong></div>
        <div class="integrity-stat"><span>Disk before</span><strong>${report.disk_files_before ?? 0}</strong></div>
        <div class="integrity-stat"><span>Disk after</span><strong>${report.disk_files_after ?? 0}</strong></div>
        <div class="integrity-stat"><span>Active OK</span><strong>${report.active_ok ?? 0}</strong></div>
        <div class="integrity-stat"><span>Recycle OK</span><strong>${report.recycle_ok ?? 0}</strong></div>
        <div class="integrity-stat"><span>Orphans</span><strong>${report.orphans_removed_count ?? 0}</strong></div>
      </div>
      ${listHtml(
        report.repaired ? 'Orphans removed' : 'Orphans',
        report.orphans_removed,
        report.orphans_removed_count,
        'integrity-warn',
      )}
      ${listHtml('Missing active', report.missing_active, report.missing_active_count, 'integrity-bad')}
      ${listHtml('Missing recycle', report.missing_recycle, report.missing_recycle_count, 'integrity-warn')}
    `;
  }

  async function checkStorageIntegrity() {
    const res = await api()('/api/v1/settings/storage/integrity');
    if (!res.ok) throw new Error(await res.text());
    const report = await res.json();
    renderIntegrityReport(report);
    showSettingsStatus(
      (report.orphans_removed_count || 0) +
        (report.missing_active_count || 0) +
        (report.missing_recycle_count || 0) ===
        0
        ? 'Storage integrity OK'
        : 'Integrity issues found',
      (report.orphans_removed_count || 0) + (report.missing_active_count || 0) > 0,
    );
  }

  async function reconcileStorage() {
    const ok = await window.MyS3UI.confirm(
      'Remove orphan files that exist on disk but have no database row? Soft-deleted files are kept. Missing blobs are reported only.',
      'Reconcile storage',
    );
    if (!ok) return;
    const res = await api()('/api/v1/settings/storage/integrity/reconcile', { method: 'POST' });
    if (!res.ok) throw new Error(await res.text());
    const report = await res.json();
    renderIntegrityReport(report);
    await loadStorage();
    showSettingsStatus(
      `Reconcile removed ${report.orphans_removed_count || 0} orphan file${
        (report.orphans_removed_count || 0) === 1 ? '' : 's'
      }`,
    );
  }

  async function changeStorageLocation() {
    const input = document.getElementById('storage-path-input');
    const path = (input.value || '').trim();
    if (!path) throw new Error('Enter a storage directory path');
    if (!storageInfo) await loadStorage();
    const same =
      storageInfo &&
      (path === storageInfo.absolute_path || path === storageInfo.path);
    if (same) {
      showSettingsStatus('Already using that path');
      return;
    }

    let mode = 'fresh';
    if (storageInfo && storageInfo.has_data) {
      const choice = await window.MyS3UI.open({
        mode: 'custom',
        title: 'Storage already has files',
        renderBody(body) {
          const p = document.createElement('p');
          p.className = 'ui-modal-message';
          p.textContent =
            'This node already stores data at the current location. Moving copies the database and object files to the new directory. Starting fresh leaves the old folder untouched and boots empty after restart.';
          body.appendChild(p);
          const pathEl = document.createElement('p');
          pathEl.className = 'ui-modal-message mono';
          pathEl.textContent = path;
          body.appendChild(pathEl);
        },
        buttons: [
          { label: 'Cancel', value: null },
          { label: 'Start fresh', value: 'fresh', danger: true },
          { label: 'Move contents', value: 'move', primary: true },
        ],
      });
      if (!choice) return;
      mode = choice;
    } else {
      const ok = await window.MyS3UI.confirm(
        `Use “${path}” as this node’s storage directory? The server will restart after saving.`,
        'Change storage location',
      );
      if (!ok) return;
      mode = 'fresh';
    }

    const btn = document.getElementById('storage-change-btn');
    if (btn) btn.disabled = true;
    try {
      const res = await api()('/api/v1/settings/storage', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ path, mode }),
      });
      if (res.status === 204) {
        showSettingsStatus('Already using that path');
        return;
      }
      if (!res.ok && res.status !== 202) throw new Error(await res.text());
      const banner = document.getElementById('storage-restart-banner');
      banner.hidden = false;
      banner.textContent =
        mode === 'move'
          ? 'Contents moved and path saved. The server is restarting — refresh after it comes back.'
          : 'New storage path saved. The server is restarting — refresh after it comes back. Old files were left in place.';
      showSettingsStatus('Storage path saved — server restarting…');
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  async function loadAccounts() {
    const [accRes, roleRes] = await Promise.all([
      api()('/api/v1/accounts'),
      api()('/api/v1/roles'),
    ]);
    if (!accRes.ok) throw new Error(await accRes.text());
    if (!roleRes.ok) throw new Error(await roleRes.text());
    accountsCache = await accRes.json();
    rolesCache = await roleRes.json();
    renderAccountsPage(accountsPage);
  }

  function renderAccountsPage(page) {
    const roleName = (id) => (rolesCache.find((r) => r.id === id) || {}).name || id;
    const me = (window.MyS3.getAuth() || {}).account;
    const myId = me && me.id != null ? me.id : null;
    const sliced = window.MyS3Pager
      ? window.MyS3Pager.slice(accountsCache, page, PAGE_SIZE)
      : { items: accountsCache, page: 1, total: accountsCache.length };
    accountsPage = sliced.page;
    const body = document.getElementById('accounts-body');
    body.innerHTML = '';
    for (const a of sliced.items) {
      const canDelete = myId != null && a.created_by_account_id === myId && a.id !== myId;
      const tr = document.createElement('tr');
      tr.innerHTML = `
        <td>${window.MyS3.esc(a.display_name || '—')}</td>
        <td class="mono">${window.MyS3.esc(a.username_hex)}</td>
        <td>${window.MyS3.esc((a.role_ids || []).map(roleName).join(', ') || '—')}</td>
        <td>${a.is_disabled ? 'Disabled' : 'Active'}</td>
        <td>
          <div class="actions">
            <button type="button" class="btn ghost" data-act="roles">Roles</button>
            <button type="button" class="btn ghost" data-act="regen">Regen password</button>
            <button type="button" class="btn ghost" data-act="toggle">${a.is_disabled ? 'Enable' : 'Disable'}</button>
            ${
              canDelete
                ? '<button type="button" class="btn ghost danger" data-act="del">Delete</button>'
                : ''
            }
          </div>
        </td>`;
      tr.querySelector('[data-act="roles"]').addEventListener('click', () => openRolesDialog(a));
      tr.querySelector('[data-act="regen"]').addEventListener('click', () => regenPassword(a.id));
      tr.querySelector('[data-act="toggle"]').addEventListener('click', () =>
        toggleDisabled(a.id, !a.is_disabled),
      );
      const del = tr.querySelector('[data-act="del"]');
      if (del) {
        del.addEventListener('click', async () => {
          const ok = await window.MyS3UI.confirmTypeDelete(
            `Permanently delete account ${a.display_name || a.username_hex}? Type delete to confirm.`,
            'Delete account',
          );
          if (!ok) return;
          const res = await api()(`/api/v1/accounts/${a.id}`, { method: 'DELETE' });
          if (!res.ok && res.status !== 204) throw new Error(await res.text());
          await loadAccounts();
        });
      }
      body.appendChild(tr);
    }
    if (window.MyS3Pager) {
      window.MyS3Pager.render(document.getElementById('accounts-pager'), {
        page: accountsPage,
        total: sliced.total,
        pageSize: PAGE_SIZE,
        onChange: renderAccountsPage,
      });
    }
  }

  async function createAccount() {
    const entered = await window.MyS3UI.prompt(
      'Display name (optional)',
      '',
      'Create account',
    );
    if (entered === null) return;
    const display_name = entered.trim();
    const res = await api()('/api/v1/accounts', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ display_name }),
    });
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    await window.MyS3UI.showCredentials(
      data.credentials.username_hex,
      data.credentials.password_hex,
    );
    await loadAccounts();
  }

  async function regenPassword(id) {
    const ok = await window.MyS3UI.confirm(
      'Regenerate password? Existing sessions will be revoked.',
      'Regenerate password',
    );
    if (!ok) return;
    const res = await api()(`/api/v1/accounts/${id}/regenerate-password`, { method: 'POST' });
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    await window.MyS3UI.showCredentials(
      data.credentials.username_hex,
      data.credentials.password_hex,
      'New credentials (shown once)',
    );
  }

  async function toggleDisabled(id, is_disabled) {
    const res = await api()(`/api/v1/accounts/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ is_disabled }),
    });
    if (!res.ok && res.status !== 204) throw new Error(await res.text());
    await loadAccounts();
  }

  function closeRolesDialog() {
    const dlg = document.getElementById('roles-dialog');
    dlg.hidden = true;
    document.body.classList.remove('ui-modal-open');
  }

  function openRolesDialog(account) {
    const dlg = document.getElementById('roles-dialog');
    document.getElementById('roles-account-id').value = String(account.id);
    const box = document.getElementById('roles-checkboxes');
    box.innerHTML = '';
    for (const r of rolesCache) {
      const label = document.createElement('label');
      label.className = 'check-item';
      const checked = (account.role_ids || []).includes(r.id);
      label.innerHTML = `<input type="checkbox" value="${r.id}" ${checked ? 'checked' : ''}/> ${window.MyS3.esc(r.name)}`;
      box.appendChild(label);
    }
    dlg.hidden = false;
    document.body.classList.add('ui-modal-open');
  }

  document.getElementById('roles-cancel').addEventListener('click', closeRolesDialog);
  document.querySelector('[data-roles-close]').addEventListener('click', closeRolesDialog);

  document.getElementById('roles-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const id = Number(document.getElementById('roles-account-id').value);
    const role_ids = Array.from(
      document.querySelectorAll('#roles-checkboxes input:checked'),
    ).map((el) => Number(el.value));
    const res = await api()(`/api/v1/accounts/${id}/roles`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ role_ids }),
    });
    if (!res.ok && res.status !== 204) {
      showSettingsStatus(await res.text(), true);
      return;
    }
    closeRolesDialog();
    await loadAccounts();
  });

  async function loadRoles() {
    const [roleRes, bucketRes] = await Promise.all([
      api()('/api/v1/roles'),
      api()('/api/v1/buckets'),
    ]);
    if (!roleRes.ok) throw new Error(await roleRes.text());
    if (!bucketRes.ok) throw new Error(await bucketRes.text());
    rolesCache = await roleRes.json();
    bucketsCache = await bucketRes.json();
    if (!selectedRoleId && rolesCache.length) selectedRoleId = rolesCache[0].id;
    renderRolesPage(rolesPage);
    if (selectedRoleId) await renderPermMatrix(selectedRoleId);
  }

  function renderRolesPage(page) {
    const sliced = window.MyS3Pager
      ? window.MyS3Pager.slice(rolesCache, page, PAGE_SIZE)
      : { items: rolesCache, page: 1, total: rolesCache.length };
    rolesPage = sliced.page;
    const list = document.getElementById('roles-list');
    list.innerHTML = '';
    for (const r of sliced.items) {
      const wrap = document.createElement('div');
      wrap.className = 'role-chip-row';
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'role-chip' + (selectedRoleId === r.id ? ' active' : '');
      btn.textContent = r.name + (r.is_owner ? ' (Owner)' : '');
      btn.addEventListener('click', () => {
        selectedRoleId = r.id;
        renderRolesPage(rolesPage);
        renderPermMatrix(r.id).catch((e) => showSettingsStatus(String(e.message || e), true));
      });
      wrap.appendChild(btn);
      if (!r.is_owner) {
        const del = document.createElement('button');
        del.type = 'button';
        del.className = 'btn ghost danger';
        del.textContent = 'Delete';
        del.addEventListener('click', async () => {
          const ok = await window.MyS3UI.confirm(`Delete role ${r.name}?`, 'Delete role');
          if (!ok) return;
          const res = await api()(`/api/v1/roles/${r.id}`, { method: 'DELETE' });
          if (!res.ok && res.status !== 204) throw new Error(await res.text());
          if (selectedRoleId === r.id) selectedRoleId = null;
          await loadRoles();
        });
        wrap.appendChild(del);
      }
      list.appendChild(wrap);
    }
    if (window.MyS3Pager) {
      window.MyS3Pager.render(document.getElementById('roles-pager'), {
        page: rolesPage,
        total: sliced.total,
        pageSize: PAGE_SIZE,
        onChange: (p) => {
          renderRolesPage(p);
        },
      });
    }
  }

  async function renderPermMatrix(roleId) {
    const matrix = document.getElementById('perm-matrix');
    matrix.hidden = false;
    const res = await api()(`/api/v1/roles/${roleId}/permissions`);
    if (!res.ok) throw new Error(await res.text());
    const perms = await res.json();
    const byBucket = new Map(perms.map((p) => [p.bucket_id, p]));
    let html = `<table class="matrix"><thead><tr><th>Bucket</th><th>C</th><th>R</th><th>U</th><th>D</th></tr></thead><tbody>`;
    for (const b of bucketsCache) {
      const p = byBucket.get(b.id) || {};
      html += `<tr data-bucket="${b.id}">
        <td>${window.MyS3.esc(b.name)}</td>
        <td><input type="checkbox" data-k="can_create" ${p.can_create ? 'checked' : ''}/></td>
        <td><input type="checkbox" data-k="can_read" ${p.can_read ? 'checked' : ''}/></td>
        <td><input type="checkbox" data-k="can_update" ${p.can_update ? 'checked' : ''}/></td>
        <td><input type="checkbox" data-k="can_delete" ${p.can_delete ? 'checked' : ''}/></td>
      </tr>`;
    }
    html += `</tbody></table>
      <button type="button" class="btn primary" id="save-perms-btn">Save permissions</button>`;
    matrix.innerHTML = html;
    document.getElementById('save-perms-btn').addEventListener('click', () => savePerms(roleId));
  }

  async function savePerms(roleId) {
    const permissions = Array.from(
      document.querySelectorAll('#perm-matrix tbody tr'),
    ).map((tr) => ({
      bucket_id: Number(tr.dataset.bucket),
      can_create: tr.querySelector('[data-k="can_create"]').checked,
      can_read: tr.querySelector('[data-k="can_read"]').checked,
      can_update: tr.querySelector('[data-k="can_update"]').checked,
      can_delete: tr.querySelector('[data-k="can_delete"]').checked,
    }));
    const res = await api()(`/api/v1/roles/${roleId}/permissions`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ permissions }),
    });
    if (!res.ok && res.status !== 204) throw new Error(await res.text());
    showSettingsStatus('Permissions saved');
  }

  async function createRole() {
    const name = document.getElementById('new-role-name').value.trim();
    if (!name) return;
    const res = await api()('/api/v1/roles', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    if (!res.ok) throw new Error(await res.text());
    document.getElementById('new-role-name').value = '';
    const role = await res.json();
    selectedRoleId = role.id;
    await loadRoles();
  }

  async function fetchAccountDirectory() {
    const res = await api()('/api/v1/accounts/directory');
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  }

  async function pickTransferOwner(currentOwnerId) {
    const directory = await fetchAccountDirectory();
    const candidates = directory.filter((a) => a.id !== currentOwnerId);
    if (!candidates.length) {
      await window.MyS3UI.alert('No other accounts available to transfer to.', 'Transfer ownership');
      return null;
    }
    /** @type {HTMLSelectElement | null} */
    let select = null;
    return window.MyS3UI.open({
      mode: 'prompt',
      title: 'Transfer ownership',
      renderBody(body) {
        const p = document.createElement('p');
        p.className = 'ui-modal-message';
        p.textContent = 'Choose the new owner for this bucket.';
        body.appendChild(p);
        select = document.createElement('select');
        select.className = 'ui-modal-input';
        for (const a of candidates) {
          const opt = document.createElement('option');
          opt.value = String(a.id);
          opt.textContent = a.display_name || `#${a.id}`;
          select.appendChild(opt);
        }
        body.appendChild(select);
      },
      buttons: [
        { label: 'Cancel', value: null },
        {
          label: 'Transfer',
          primary: true,
          getValue: () => (select ? Number(select.value) : null),
        },
      ],
    });
  }

  async function loadBuckets() {
    const [bucketRes, dirRes] = await Promise.all([
      api()('/api/v1/buckets'),
      api()('/api/v1/accounts/directory'),
    ]);
    if (!bucketRes.ok) throw new Error(await bucketRes.text());
    bucketsCache = await bucketRes.json();
    directoryCache = dirRes.ok ? await dirRes.json() : [];
    renderBucketsPage(bucketsPage);
  }

  function renderBucketsPage(page) {
    const ownerName = (id) => {
      if (id == null) return null;
      const row = directoryCache.find((a) => a.id === id);
      return row ? row.display_name : `#${id}`;
    };
    const sliced = window.MyS3Pager
      ? window.MyS3Pager.slice(bucketsCache, page, PAGE_SIZE)
      : { items: bucketsCache, page: 1, total: bucketsCache.length };
    bucketsPage = sliced.page;
    const ul = document.getElementById('bucket-list');
    ul.innerHTML = '';
    const current = window.MyS3.getBucket();
    const me = (window.MyS3.getAuth() || {}).account;
    const myId = me && me.id != null ? me.id : null;
    for (const b of sliced.items) {
      const li = document.createElement('li');
      li.className = 'bucket-item' + (b.name === current ? ' active' : '');
      const isBucketOwner = myId != null && b.owner_account_id === myId;
      const ownerLabel = isBucketOwner
        ? 'Owner: you'
        : b.owner_account_id != null
          ? `Owner: ${ownerName(b.owner_account_id)}`
          : 'No owner';
      const chips = [ownerLabel];
      if (b.name === 'storage') chips.push('Default');
      const canRename = isBucketOwner && b.name !== 'storage';
      const canTransfer = isBucketOwner;
      const canDelete = isBucketOwner && b.name !== 'storage';
      const canEditNodes = !!b.can_edit_replication;
      li.innerHTML = `
        <div class="bucket-meta">
          <span class="mono">${window.MyS3.esc(b.name)}</span>
          ${chips
            .map((c) => `<span class="bucket-owner-chip">${window.MyS3.esc(c)}</span>`)
            .join('')}
        </div>
        <div class="actions">
          <button type="button" class="btn ghost" data-act="use">Use in explorer</button>
          ${
            canEditNodes
              ? '<button type="button" class="btn ghost" data-act="nodes">Settings</button>'
              : ''
          }
          ${
            canRename
              ? '<button type="button" class="btn ghost" data-act="rename">Rename</button>'
              : ''
          }
          ${
            canTransfer
              ? '<button type="button" class="btn ghost" data-act="transfer">Transfer</button>'
              : ''
          }
          ${
            canDelete
              ? '<button type="button" class="btn ghost danger" data-act="del">Delete</button>'
              : ''
          }
        </div>`;
      li.querySelector('[data-act="use"]').addEventListener('click', () => {
        window.MyS3.setBucket(b.name);
        location.hash = '#/explore/';
      });
      const nodesBtn = li.querySelector('[data-act="nodes"]');
      if (nodesBtn) {
        nodesBtn.addEventListener('click', () => {
          openNodesDialog(b).catch((e) =>
            showSettingsStatus(String(e.message || e), true),
          );
        });
      }
      const renameBtn = li.querySelector('[data-act="rename"]');
      if (renameBtn) {
        renameBtn.addEventListener('click', () => {
          (async () => {
            const entered = await window.MyS3UI.prompt(
              'New bucket name',
              b.name,
              'Rename bucket',
            );
            if (entered === null) return;
            const name = entered.trim();
            if (!name || name === b.name) return;
            const r = await api()(`/api/v1/buckets/${b.id}`, {
              method: 'PATCH',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ name }),
            });
            if (!r.ok) throw new Error(await r.text());
            if (window.MyS3.getBucket() === b.name) {
              window.MyS3.setBucket(name);
            }
            await loadBuckets();
          })().catch((e) => showSettingsStatus(String(e.message || e), true));
        });
      }
      const transferBtn = li.querySelector('[data-act="transfer"]');
      if (transferBtn) {
        transferBtn.addEventListener('click', () => {
          (async () => {
            const newOwnerId = await pickTransferOwner(b.owner_account_id);
            if (newOwnerId == null || Number.isNaN(newOwnerId)) return;
            const ok = await window.MyS3UI.confirm(
              `Transfer ownership of "${b.name}"? You will no longer be able to rename, transfer, or delete this bucket.`,
              'Transfer ownership',
            );
            if (!ok) return;
            const r = await api()(`/api/v1/buckets/${b.id}`, {
              method: 'PATCH',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ owner_account_id: newOwnerId }),
            });
            if (!r.ok) throw new Error(await r.text());
            await loadBuckets();
          })().catch((e) => showSettingsStatus(String(e.message || e), true));
        });
      }
      const del = li.querySelector('[data-act="del"]');
      if (del) {
        del.addEventListener('click', () => {
          (async () => {
            const ok = await window.MyS3UI.confirmTypeDelete(
              `Permanently delete bucket "${b.name}" and all objects in it (including recycle bin)? Type delete to confirm.`,
              'Delete bucket',
            );
            if (!ok) return;
            const r = await api()(`/api/v1/buckets/${b.id}`, { method: 'DELETE' });
            if (!r.ok && r.status !== 204) throw new Error(await r.text());
            await loadBuckets();
          })().catch((e) => showSettingsStatus(String(e.message || e), true));
        });
      }
      ul.appendChild(li);
    }
    if (window.MyS3Pager) {
      window.MyS3Pager.render(document.getElementById('buckets-pager'), {
        page: bucketsPage,
        total: sliced.total,
        pageSize: PAGE_SIZE,
        onChange: renderBucketsPage,
      });
    }
  }

  async function createBucket() {
    const name = document.getElementById('new-bucket-name').value.trim();
    if (!name) return;
    const res = await api()('/api/v1/buckets', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }),
    });
    if (!res.ok) throw new Error(await res.text());
    document.getElementById('new-bucket-name').value = '';
    await loadBuckets();
  }

  function closeNodesDialog() {
    stopEtagRehashPoll();
    const dlg = document.getElementById('nodes-dialog');
    dlg.hidden = true;
    document.body.classList.remove('ui-modal-open');
    editingNodeId = null;
  }

  function formatGiB(bytes) {
    const n = Number(bytes) || 0;
    const gib = n / (1024 * 1024 * 1024);
    if (gib >= 10) return gib.toFixed(0) + ' GB';
    if (gib >= 1) return gib.toFixed(1) + ' GB';
    const mib = n / (1024 * 1024);
    if (mib >= 1) return mib.toFixed(1) + ' MB';
    return window.MyS3.formatBytes(n);
  }

  function bytesToGbInput(bytes) {
    return Math.round(((Number(bytes) || 0) / (1024 * 1024 * 1024)) * 1000) / 1000;
  }

  function showNodesStatus(msg, isError) {
    const el = document.getElementById('nodes-dialog-status');
    if (!el) return;
    if (!msg) {
      el.hidden = true;
      el.textContent = '';
      return;
    }
    el.hidden = false;
    el.textContent = msg;
    el.classList.toggle('error', !!isError);
  }

  /** @type {number|null} */
  let nodesDialogBucketId = null;
  /** @type {string|null} */
  let editingNodeId = null;
  /** @type {ReturnType<typeof setInterval>|null} */
  let etagRehashPollTimer = null;

  function stopEtagRehashPoll() {
    if (etagRehashPollTimer != null) {
      clearInterval(etagRehashPollTimer);
      etagRehashPollTimer = null;
    }
  }

  function updateEtagSection(data) {
    const typeSel = document.getElementById('nodes-etag-type');
    if (typeSel && data.etag_type) {
      typeSel.value = data.etag_type;
    }
    const progress = document.getElementById('nodes-etag-progress');
    const applyBtn = document.getElementById('nodes-etag-apply-btn');
    const status = data.etag_rehash_status || null;
    const processed = Number(data.etag_rehash_processed) || 0;
    const total = Number(data.etag_rehash_total) || 0;
    if (status === 'running') {
      progress.hidden = false;
      progress.textContent = `Rehashing ${processed} / ${total}…`;
      progress.classList.remove('error');
      if (applyBtn) applyBtn.disabled = true;
      if (etagRehashPollTimer == null && nodesDialogBucketId != null) {
        etagRehashPollTimer = setInterval(() => {
          refreshNodesDialog().catch((e) => showNodesStatus(String(e.message || e), true));
        }, 2000);
      }
    } else if (status === 'error') {
      stopEtagRehashPoll();
      progress.hidden = false;
      progress.textContent = data.etag_rehash_error
        ? `Rehash failed: ${data.etag_rehash_error}`
        : 'Rehash failed';
      progress.classList.add('error');
      if (applyBtn) applyBtn.disabled = false;
    } else if (status === 'done') {
      stopEtagRehashPoll();
      progress.hidden = false;
      progress.textContent = `Rehash complete (${processed} / ${total})`;
      progress.classList.remove('error');
      if (applyBtn) applyBtn.disabled = false;
    } else {
      stopEtagRehashPoll();
      progress.hidden = true;
      progress.textContent = '';
      progress.classList.remove('error');
      if (applyBtn) applyBtn.disabled = false;
    }
  }

  async function applyBucketEtag() {
    if (nodesDialogBucketId == null) return;
    const etag_type = document.getElementById('nodes-etag-type').value;
    const apply = document.getElementById('nodes-etag-apply').value || 'new_only';
    if (apply === 'recalculate_all') {
      const ok = await window.MyS3UI.confirm(
        'Recalculate ETags for every object in this bucket? This may take a while and will update replication digests.',
        'Recalculate ETags',
      );
      if (!ok) return;
    }
    const res = await api()(`/api/v1/buckets/${nodesDialogBucketId}/etag`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ etag_type, apply }),
    });
    if (!res.ok && res.status !== 204 && res.status !== 202) {
      throw new Error(await res.text());
    }
    if (apply === 'recalculate_all') {
      showNodesStatus(`Recalculating with ${etag_type}…`);
    } else {
      showNodesStatus(`ETag algorithm set to ${etag_type} (new objects only)`);
    }
    showSettingsStatus(`ETag algorithm: ${etag_type}`);
    await refreshNodesDialog();
  }

  function renderNodeCard(n, used, nodeCount) {
    const pct = n.allocated_bytes > 0
      ? Math.min(100, Math.round((used / n.allocated_bytes) * 100))
      : 0;
    const over = used > n.allocated_bytes;
    const hard = n.quota_mode === 'hard';
    const canRemove = !n.is_local && nodeCount > 1;
    const isEditing = editingNodeId === n.id;
    const card = document.createElement('article');
    card.className = 'nodes-card' + (n.is_local ? ' is-local' : '') + (isEditing ? ' is-editing' : '');
    card.dataset.nodeId = n.id;

    const storageLine = n.is_local && nodesLocalStoragePath
      ? `<div class="nodes-storage-line muted">
          Storage: <span class="mono">${window.MyS3.esc(nodesLocalStoragePath)}</span>
          <button type="button" class="btn ghost nodes-storage-link" data-act="storage">Manage in Settings</button>
        </div>`
      : '';

    const allocTitle = `${formatGiB(used)} used of ${formatGiB(n.allocated_bytes)}`;
    card.innerHTML = `
      <div class="nodes-card-main">
        <div class="nodes-card-identity">
          <div class="nodes-card-title-row">
            <span class="nodes-card-id mono">${window.MyS3.esc(n.id)}</span>
            ${n.is_local ? '<span class="nodes-badge local">Local</span>' : '<span class="nodes-badge remote">Remote</span>'}
            <span class="nodes-mode-chip ${hard ? 'hard' : 'soft'}">${hard ? 'Hard' : 'Soft'}</span>
          </div>
          <div class="nodes-card-endpoint mono muted">${window.MyS3.esc(n.endpoint || '—')}</div>
          ${storageLine}
        </div>
        <div class="nodes-card-alloc" title="${window.MyS3.esc(allocTitle)}">
          <div class="nodes-alloc-meta">
            <span>${window.MyS3.esc(formatGiB(used))} used</span>
            <span>${pct}% of ${window.MyS3.esc(formatGiB(n.allocated_bytes))}</span>
          </div>
          <div class="nodes-alloc-bar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${pct}">
            <i style="width:${pct}%" class="${over ? 'over' : hard ? 'hard' : ''}"></i>
          </div>
        </div>
        <div class="nodes-card-actions">
          <button type="button" class="btn ghost" data-act="edit">${isEditing ? 'Cancel' : 'Edit'}</button>
          <button type="button" class="btn ghost danger" data-act="remove" ${
            canRemove ? '' : 'disabled'
          } title="${
            n.is_local
              ? 'Local node cannot be removed'
              : nodeCount <= 1
                ? 'Keep at least one node'
                : 'Remove from bucket'
          }">Remove</button>
        </div>
      </div>
      <div class="nodes-card-edit" ${isEditing ? '' : 'hidden'}>
        <label class="field">
          Capacity (GB)
          <input type="number" class="nodes-edit-gb" min="0.1" step="1" value="${bytesToGbInput(n.allocated_bytes) || 100}" />
        </label>
        <div class="field">
          <span class="field-label">Quota</span>
          <div class="nodes-mode-toggle" role="group" aria-label="Quota mode">
            <button type="button" class="nodes-mode-opt ${hard ? '' : 'is-active'}" data-mode="soft">Soft</button>
            <button type="button" class="nodes-mode-opt ${hard ? 'is-active' : ''}" data-mode="hard">Hard</button>
          </div>
          <input type="hidden" class="nodes-edit-mode" value="${hard ? 'hard' : 'soft'}" />
          <p class="nodes-mode-help muted">Soft shows usage only. Hard blocks uploads past the allocation.</p>
        </div>
        <div class="nodes-card-edit-actions">
          <button type="button" class="btn ghost" data-act="cancel-edit">Cancel</button>
          <button type="button" class="btn primary" data-act="save">Save</button>
        </div>
      </div>`;

    const editPanel = card.querySelector('.nodes-card-edit');
    const modeToggle = editPanel.querySelector('.nodes-mode-toggle');
    const modeHidden = editPanel.querySelector('.nodes-edit-mode');
    modeToggle.querySelectorAll('.nodes-mode-opt').forEach((btn) => {
      btn.addEventListener('click', () => {
        modeHidden.value = btn.dataset.mode;
        modeToggle.querySelectorAll('.nodes-mode-opt').forEach((b) => {
          b.classList.toggle('is-active', b.dataset.mode === btn.dataset.mode);
        });
      });
    });

    card.querySelector('[data-act="edit"]').addEventListener('click', () => {
      editingNodeId = isEditing ? null : n.id;
      refreshNodesDialog().catch((e) => showNodesStatus(String(e.message || e), true));
    });
    card.querySelector('[data-act="cancel-edit"]').addEventListener('click', () => {
      editingNodeId = null;
      refreshNodesDialog().catch((e) => showNodesStatus(String(e.message || e), true));
    });
    card.querySelector('[data-act="save"]').addEventListener('click', () => {
      saveNodeAssignment(n.id, card).catch((e) => showNodesStatus(String(e.message || e), true));
    });
    const storageBtn = card.querySelector('[data-act="storage"]');
    if (storageBtn) {
      storageBtn.addEventListener('click', () => {
        closeNodesDialog();
        location.hash = '#/settings/storage';
      });
    }
    const removeBtn = card.querySelector('[data-act="remove"]');
    if (removeBtn && !removeBtn.disabled) {
      removeBtn.addEventListener('click', () => {
        removeNodeAssignment(n.id).catch((e) => showNodesStatus(String(e.message || e), true));
      });
    }
    return card;
  }

  async function refreshNodesDialog() {
    if (nodesDialogBucketId == null) return;
    const res = await api()(`/api/v1/buckets/${nodesDialogBucketId}/nodes`);
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    const used = data.used_bytes || 0;
    nodesLocalStoragePath = data.local_storage_path || '';
    const usageEl = document.getElementById('nodes-usage-summary');
    usageEl.innerHTML = `
      <span class="nodes-usage-label">Bucket used</span>
      <strong class="nodes-usage-value">${window.MyS3.esc(formatGiB(used))}</strong>
      <span class="nodes-usage-meta muted">${(data.nodes || []).length} node${
        (data.nodes || []).length === 1 ? '' : 's'
      }</span>`;

    const list = document.getElementById('nodes-list');
    list.innerHTML = '';
    const nodes = data.nodes || [];
    if (!nodes.length) {
      const empty = document.createElement('div');
      empty.className = 'nodes-empty-hint';
      empty.innerHTML = '<p>No nodes assigned yet.</p>';
      list.appendChild(empty);
    } else {
      for (const n of nodes) {
        list.appendChild(renderNodeCard(n, used, nodes.length));
      }
    }

    const available = data.available || [];
    const pickWrap = document.getElementById('nodes-add-pick-wrap');
    const select = document.getElementById('nodes-add-peer');
    const prev = select.value;
    select.innerHTML = '';
    const blank = document.createElement('option');
    blank.value = '';
    blank.textContent = 'Choose a discovered peer…';
    select.appendChild(blank);
    if (available.length) {
      pickWrap.hidden = false;
      for (const p of available) {
        const opt = document.createElement('option');
        opt.value = p.id;
        opt.dataset.endpoint = p.endpoint || '';
        opt.textContent = `${p.id} · ${p.endpoint || 'no endpoint'}`;
        select.appendChild(opt);
      }
      if (prev && [...select.options].some((o) => o.value === prev)) {
        select.value = prev;
      }
    } else {
      pickWrap.hidden = true;
    }

    updateEtagSection(data);
  }

  async function openNodesDialog(bucket) {
    stopEtagRehashPoll();
    nodesDialogBucketId = bucket.id;
    editingNodeId = null;
    showNodesStatus('');
    document.getElementById('nodes-bucket-id').value = String(bucket.id);
    document.getElementById('nodes-bucket-label').innerHTML =
      `Capacity & placement for <strong class="mono">${window.MyS3.esc(bucket.name)}</strong>`;
    document.getElementById('nodes-add-mode').value = 'soft';
    document.getElementById('nodes-add-endpoint').value = '';
    document.getElementById('nodes-add-node-id').value = '';
    const addPeer = document.getElementById('nodes-add-peer');
    if (addPeer) addPeer.value = '';
    const addToggle = document.querySelector('#nodes-add-form .nodes-mode-toggle');
    if (addToggle) {
      addToggle.querySelectorAll('.nodes-mode-opt').forEach((btn) => {
        btn.classList.toggle('is-active', btn.dataset.mode === 'soft');
      });
    }
    document.getElementById('nodes-etag-apply').value = 'new_only';
    document.querySelectorAll('#nodes-etag-section .nodes-etag-apply .nodes-mode-opt').forEach((btn) => {
      btn.classList.toggle('is-active', btn.dataset.apply === 'new_only');
    });
    await refreshNodesDialog();
    const dlg = document.getElementById('nodes-dialog');
    dlg.hidden = false;
    document.body.classList.add('ui-modal-open');
  }

  async function saveNodeAssignment(nodeId, card) {
    const allocated_gb = Number(card.querySelector('.nodes-edit-gb').value);
    const quota_mode = card.querySelector('.nodes-edit-mode').value;
    if (!Number.isFinite(allocated_gb) || allocated_gb <= 0) {
      throw new Error('Enter a positive GB value');
    }
    const res = await api()(
      `/api/v1/buckets/${nodesDialogBucketId}/nodes/${encodeURIComponent(nodeId)}`,
      {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ allocated_gb, quota_mode }),
      },
    );
    if (!res.ok && res.status !== 204) throw new Error(await res.text());
    editingNodeId = null;
    showNodesStatus(`Updated ${nodeId}`);
    showSettingsStatus(`Updated ${nodeId}`);
    await refreshNodesDialog();
  }

  async function removeNodeAssignment(nodeId) {
    const ok = await window.MyS3UI.confirm(
      `Remove node “${nodeId}” from this bucket?`,
      'Remove node',
    );
    if (!ok) return;
    const res = await api()(
      `/api/v1/buckets/${nodesDialogBucketId}/nodes/${encodeURIComponent(nodeId)}`,
      { method: 'DELETE' },
    );
    if (!res.ok && res.status !== 204) throw new Error(await res.text());
    showNodesStatus(`Removed ${nodeId}`);
    showSettingsStatus(`Removed ${nodeId}`);
    await refreshNodesDialog();
  }

  async function addNodeAssignment() {
    const endpoint = document.getElementById('nodes-add-endpoint').value.trim();
    const node_id = document.getElementById('nodes-add-node-id').value.trim();
    const allocated_gb = Number(document.getElementById('nodes-add-gb').value);
    const quota_mode = document.getElementById('nodes-add-mode').value;
    if (!endpoint && !node_id) throw new Error('Enter a peer URL or pick an existing peer');
    if (!Number.isFinite(allocated_gb) || allocated_gb <= 0) {
      throw new Error('Enter a positive GB value');
    }
    const body = { allocated_gb, quota_mode };
    if (endpoint) body.endpoint = endpoint;
    if (node_id) body.node_id = node_id;
    const res = await api()(`/api/v1/buckets/${nodesDialogBucketId}/nodes`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (!res.ok && res.status !== 204) throw new Error(await res.text());
    const label = node_id || endpoint;
    showNodesStatus(`Added ${label}`);
    showSettingsStatus(`Added ${label}`);
    document.getElementById('nodes-add-endpoint').value = '';
    document.getElementById('nodes-add-node-id').value = '';
    document.getElementById('nodes-add-peer').value = '';
    document.getElementById('nodes-add-gb').value = '100';
    document.getElementById('nodes-add-mode').value = 'soft';
    const addToggle = document.querySelector('#nodes-add-form .nodes-mode-toggle');
    if (addToggle) {
      addToggle.querySelectorAll('.nodes-mode-opt').forEach((btn) => {
        btn.classList.toggle('is-active', btn.dataset.mode === 'soft');
      });
    }
    await refreshNodesDialog();
  }

  function onPickExistingPeer() {
    const select = document.getElementById('nodes-add-peer');
    const opt = select.selectedOptions[0];
    if (!opt || !opt.value) return;
    document.getElementById('nodes-add-node-id').value = opt.value;
    document.getElementById('nodes-add-endpoint').value = opt.dataset.endpoint || '';
  }

  function updateRecycleSelectionUi() {
    const boxes = Array.from(document.querySelectorAll('#recycle-body input[data-recycle-id]'));
    const checked = boxes.filter((b) => b.checked);
    const selectAll = document.getElementById('recycle-select-all');
    const deleteBtn = document.getElementById('recycle-delete-selected');
    const emptyAllBtn = document.getElementById('recycle-empty-all');
    const countEl = document.getElementById('recycle-selection-count');
    const bulk = document.getElementById('recycle-bulk-actions');
    if (selectAll) {
      selectAll.checked = boxes.length > 0 && checked.length === boxes.length;
      selectAll.indeterminate = checked.length > 0 && checked.length < boxes.length;
      selectAll.disabled = boxes.length === 0;
    }
    if (deleteBtn) deleteBtn.disabled = checked.length === 0;
    if (emptyAllBtn) emptyAllBtn.disabled = !recycleCache.length;
    if (countEl) {
      countEl.textContent =
        checked.length > 0 ? `${checked.length} selected` : '';
    }
    if (bulk) bulk.hidden = boxes.length === 0;
  }

  function selectedRecycleIds() {
    return Array.from(document.querySelectorAll('#recycle-body input[data-recycle-id]:checked')).map(
      (el) => Number(el.getAttribute('data-recycle-id')),
    );
  }

  async function purgeRecycleIds(ids) {
    if (!ids.length) return;
    const ok = await window.MyS3UI.confirmTypeDelete(
      `Permanently delete ${ids.length} object${ids.length === 1 ? '' : 's'}? This cannot be undone.`,
      'Delete forever',
    );
    if (!ok) return;
    const res = await api()('/api/v1/recycle-bin/purge', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids }),
    });
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    const failed = (data.failed || []).length;
    showSettingsStatus(
      failed
        ? `Deleted ${data.deleted}; ${failed} failed`
        : `Permanently deleted ${data.deleted} object${data.deleted === 1 ? '' : 's'}`,
      failed > 0,
    );
    await loadRecycle();
  }

  async function emptyRecycleBin() {
    if (!recycleCache.length) return;
    const ok = await window.MyS3UI.confirmTypeDelete(
      `Permanently delete all ${recycleCache.length} object${
        recycleCache.length === 1 ? '' : 's'
      } in the recycle bin? This cannot be undone.`,
      'Empty recycle bin',
    );
    if (!ok) return;
    const res = await api()('/api/v1/recycle-bin/purge-all', { method: 'POST' });
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    const failed = (data.failed || []).length;
    showSettingsStatus(
      failed
        ? `Emptied ${data.deleted}; ${failed} failed`
        : `Emptied recycle bin (${data.deleted} deleted)`,
      failed > 0,
    );
    await loadRecycle();
  }

  async function loadRecycle() {
    const auth = window.MyS3.getAuth();
    if (auth.is_owner) {
      const sRes = await api()('/api/v1/settings/recycle');
      if (sRes.ok) {
        const s = await sRes.json();
        document.getElementById('retention-value').value = s.recycle_retention_value;
        document.getElementById('retention-unit').value = s.recycle_retention_unit;
        document.getElementById('retention-form').hidden = false;
      }
    } else {
      document.getElementById('retention-form').hidden = true;
    }

    const res = await api()('/api/v1/recycle-bin');
    if (!res.ok) throw new Error(await res.text());
    recycleCache = await res.json();
    if (!bucketsCache.length) {
      const bRes = await api()('/api/v1/buckets');
      if (bRes.ok) bucketsCache = await bRes.json();
    }
    renderRecyclePage(recyclePage);
  }

  function renderRecyclePage(page) {
    const bucketName = (id) => (bucketsCache.find((b) => b.id === id) || {}).name || id;
    const sliced = window.MyS3Pager
      ? window.MyS3Pager.slice(recycleCache, page, PAGE_SIZE)
      : { items: recycleCache, page: 1, total: recycleCache.length };
    recyclePage = sliced.page;
    const body = document.getElementById('recycle-body');
    body.innerHTML = '';
    const selectAll = document.getElementById('recycle-select-all');
    if (selectAll) {
      selectAll.checked = false;
      selectAll.indeterminate = false;
    }
    for (const o of sliced.items) {
      const tr = document.createElement('tr');
      tr.innerHTML = `
        <td class="col-check">
          <input type="checkbox" data-recycle-id="${o.id}" aria-label="Select ${window.MyS3.escAttr(o.original_filename)}" />
        </td>
        <td class="mono">${window.MyS3.esc(o.original_filename)}</td>
        <td class="mono">${window.MyS3.esc(String(bucketName(o.bucket_id)))}</td>
        <td class="mono">${window.MyS3.formatDate(o.deleted_at)}</td>
        <td class="mono">${window.MyS3.formatBytes(o.filesize_bytes)}</td>
        <td>
          <div class="actions">
            <button type="button" class="btn ghost" data-act="restore">Restore</button>
            <button type="button" class="btn ghost danger" data-act="purge">Delete forever</button>
          </div>
        </td>`;
      tr.querySelector('input[data-recycle-id]').addEventListener('change', updateRecycleSelectionUi);
      tr.querySelector('[data-act="restore"]').addEventListener('click', async () => {
        const r = await api()(`/api/v1/recycle-bin/${o.id}/restore`, { method: 'POST' });
        if (!r.ok) throw new Error(await r.text());
        showSettingsStatus('Restored');
        await loadRecycle();
      });
      tr.querySelector('[data-act="purge"]').addEventListener('click', async () => {
        try {
          await purgeRecycleIds([o.id]);
        } catch (err) {
          showSettingsStatus(String(err.message || err), true);
        }
      });
      body.appendChild(tr);
    }
    document.getElementById('recycle-empty').hidden = sliced.total > 0;
    updateRecycleSelectionUi();
    if (window.MyS3Pager) {
      window.MyS3Pager.render(document.getElementById('recycle-pager'), {
        page: recyclePage,
        total: sliced.total,
        pageSize: PAGE_SIZE,
        onChange: renderRecyclePage,
      });
    }
  }

  document.getElementById('create-account-btn').addEventListener('click', () => {
    createAccount().catch((e) => showSettingsStatus(String(e.message || e), true));
  });
  document.getElementById('create-role-btn').addEventListener('click', () => {
    createRole().catch((e) => showSettingsStatus(String(e.message || e), true));
  });
  document.getElementById('create-bucket-btn').addEventListener('click', () => {
    createBucket().catch((e) => showSettingsStatus(String(e.message || e), true));
  });
  document.getElementById('retention-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      const res = await api()('/api/v1/settings/recycle', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          recycle_retention_value: Number(document.getElementById('retention-value').value),
          recycle_retention_unit: document.getElementById('retention-unit').value,
        }),
      });
      if (!res.ok) throw new Error(await res.text());
      showSettingsStatus('Retention saved');
    } catch (err) {
      showSettingsStatus(String(err.message || err), true);
    }
  });

  document.getElementById('recycle-select-all').addEventListener('change', (e) => {
    const on = e.target.checked;
    document.querySelectorAll('#recycle-body input[data-recycle-id]').forEach((box) => {
      box.checked = on;
    });
    updateRecycleSelectionUi();
  });

  document.getElementById('recycle-delete-selected').addEventListener('click', () => {
    purgeRecycleIds(selectedRecycleIds()).catch((err) =>
      showSettingsStatus(String(err.message || err), true),
    );
  });

  document.getElementById('recycle-empty-all').addEventListener('click', () => {
    emptyRecycleBin().catch((err) =>
      showSettingsStatus(String(err.message || err), true),
    );
  });

  document.getElementById('nodes-close').addEventListener('click', closeNodesDialog);
  document.querySelector('[data-nodes-close]').addEventListener('click', closeNodesDialog);
  document.querySelectorAll('#nodes-add-form .nodes-mode-opt').forEach((btn) => {
    btn.addEventListener('click', () => {
      document.getElementById('nodes-add-mode').value = btn.dataset.mode;
      document.querySelectorAll('#nodes-add-form .nodes-mode-opt').forEach((b) => {
        b.classList.toggle('is-active', b.dataset.mode === btn.dataset.mode);
      });
    });
  });
  document.querySelectorAll('#nodes-etag-section .nodes-etag-apply .nodes-mode-opt').forEach((btn) => {
    btn.addEventListener('click', () => {
      document.getElementById('nodes-etag-apply').value = btn.dataset.apply;
      document.querySelectorAll('#nodes-etag-section .nodes-etag-apply .nodes-mode-opt').forEach((b) => {
        b.classList.toggle('is-active', b.dataset.apply === btn.dataset.apply);
      });
    });
  });
  document.getElementById('nodes-add-btn').addEventListener('click', () => {
    addNodeAssignment().catch((err) => {
      showNodesStatus(String(err.message || err), true);
      showSettingsStatus(String(err.message || err), true);
    });
  });
  document.getElementById('nodes-add-peer').addEventListener('change', onPickExistingPeer);
  document.getElementById('nodes-etag-apply-btn').addEventListener('click', () => {
    applyBucketEtag().catch((err) => {
      showNodesStatus(String(err.message || err), true);
      showSettingsStatus(String(err.message || err), true);
    });
  });

  const storagePathInput = document.getElementById('storage-path-input');
  if (storagePathInput) {
    storagePathInput.addEventListener('input', () => {
      storagePathInput.dataset.touched = '1';
    });
  }
  const storageChangeBtn = document.getElementById('storage-change-btn');
  if (storageChangeBtn) {
    storageChangeBtn.addEventListener('click', () => {
      changeStorageLocation().catch((err) =>
        showSettingsStatus(String(err.message || err), true),
      );
    });
  }
  const storageCheckBtn = document.getElementById('storage-check-btn');
  if (storageCheckBtn) {
    storageCheckBtn.addEventListener('click', () => {
      checkStorageIntegrity().catch((err) =>
        showSettingsStatus(String(err.message || err), true),
      );
    });
  }
  const storageReconcileBtn = document.getElementById('storage-reconcile-btn');
  if (storageReconcileBtn) {
    storageReconcileBtn.addEventListener('click', () => {
      reconcileStorage().catch((err) =>
        showSettingsStatus(String(err.message || err), true),
      );
    });
  }

  window.MyS3Settings = {
    showPanel,
    onAuth(auth) {
      const ownerOnly = ['accounts', 'roles', 'storage'];
      navLinks.forEach((a) => {
        const panel = a.dataset.panel;
        if (ownerOnly.includes(panel)) {
          a.hidden = !auth.is_owner;
        }
      });
      document.getElementById('create-bucket-btn').hidden = !auth.is_owner;
      document.getElementById('new-bucket-name').hidden = !auth.is_owner;
    },
  };
})();
