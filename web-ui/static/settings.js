(function () {
  const settingsStatus = document.getElementById('settings-status');
  const panels = {
    accounts: document.getElementById('panel-accounts'),
    roles: document.getElementById('panel-roles'),
    buckets: document.getElementById('panel-buckets'),
    recycle: document.getElementById('panel-recycle'),
  };
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
      if (key === 'recycle') await loadRecycle();
    } catch (err) {
      showSettingsStatus(String(err.message || err), true);
    }
  }

  async function loadAccounts() {
    const [accRes, roleRes] = await Promise.all([
      api()('/api/v1/accounts'),
      api()('/api/v1/roles'),
    ]);
    if (!accRes.ok) throw new Error(await accRes.text());
    if (!roleRes.ok) throw new Error(await roleRes.text());
    const accounts = await accRes.json();
    rolesCache = await roleRes.json();
    const roleName = (id) => (rolesCache.find((r) => r.id === id) || {}).name || id;
    const me = (window.MyS3.getAuth() || {}).account;
    const myId = me && me.id != null ? me.id : null;
    const body = document.getElementById('accounts-body');
    body.innerHTML = '';
    for (const a of accounts) {
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
    const list = document.getElementById('roles-list');
    list.innerHTML = '';
    for (const r of rolesCache) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'role-chip' + (selectedRoleId === r.id ? ' active' : '');
      btn.textContent = r.name + (r.is_owner ? ' (Owner)' : '');
      btn.addEventListener('click', () => {
        selectedRoleId = r.id;
        loadRoles();
      });
      list.appendChild(btn);
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
        list.appendChild(del);
      }
    }
    if (!selectedRoleId && rolesCache.length) selectedRoleId = rolesCache[0].id;
    if (selectedRoleId) await renderPermMatrix(selectedRoleId);
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
    const directory = dirRes.ok ? await dirRes.json() : [];
    const ownerName = (id) => {
      if (id == null) return null;
      const row = directory.find((a) => a.id === id);
      return row ? row.display_name : `#${id}`;
    };
    const ul = document.getElementById('bucket-list');
    ul.innerHTML = '';
    const current = window.MyS3.getBucket();
    const me = (window.MyS3.getAuth() || {}).account;
    const myId = me && me.id != null ? me.id : null;
    for (const b of bucketsCache) {
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
      const canEditReplication = !!b.can_edit_replication;
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
            canEditReplication
              ? '<button type="button" class="btn ghost" data-act="repl">Settings</button>'
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
      const replBtn = li.querySelector('[data-act="repl"]');
      if (replBtn) {
        replBtn.addEventListener('click', () => {
          openReplicationDialog(b).catch((e) =>
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

  function closeReplicationDialog() {
    const dlg = document.getElementById('replication-dialog');
    dlg.hidden = true;
    document.body.classList.remove('ui-modal-open');
  }

  function syncReplicationPeerUi() {
    const all = document.getElementById('replication-all-nodes').checked;
    const box = document.getElementById('replication-peer-checkboxes');
    box.querySelectorAll('input[type="checkbox"]').forEach((el) => {
      el.disabled = all;
    });
    box.style.opacity = all ? '0.55' : '1';
  }

  async function openReplicationDialog(bucket) {
    const res = await api()(`/api/v1/buckets/${bucket.id}/replication`);
    if (!res.ok) throw new Error(await res.text());
    const data = await res.json();
    document.getElementById('replication-bucket-id').value = String(bucket.id);
    document.getElementById('replication-bucket-label').textContent =
      `Configure which cluster nodes receive objects from “${bucket.name}”.`;
    const allEl = document.getElementById('replication-all-nodes');
    allEl.checked = !!data.replicate_to_all;
    const selected = new Set(data.peer_ids || []);
    const box = document.getElementById('replication-peer-checkboxes');
    box.innerHTML = '';
    const peers = data.peers || [];
    const hint = document.getElementById('replication-peers-hint');
    hint.hidden = peers.length > 0;
    for (const p of peers) {
      const label = document.createElement('label');
      label.className = 'check-item';
      const checked = selected.has(p.id);
      label.innerHTML = `<input type="checkbox" value="${window.MyS3.esc(p.id)}" ${
        checked ? 'checked' : ''
      }/> <span class="mono">${window.MyS3.esc(p.id)}</span> <span class="muted">${window.MyS3.esc(
        p.endpoint || '',
      )}</span>`;
      box.appendChild(label);
    }
    syncReplicationPeerUi();
    const dlg = document.getElementById('replication-dialog');
    dlg.hidden = false;
    document.body.classList.add('ui-modal-open');
  }

  function updateRecycleSelectionUi() {
    const boxes = Array.from(document.querySelectorAll('#recycle-body input[data-recycle-id]'));
    const checked = boxes.filter((b) => b.checked);
    const selectAll = document.getElementById('recycle-select-all');
    const deleteBtn = document.getElementById('recycle-delete-selected');
    const countEl = document.getElementById('recycle-selection-count');
    const bulk = document.getElementById('recycle-bulk-actions');
    if (selectAll) {
      selectAll.checked = boxes.length > 0 && checked.length === boxes.length;
      selectAll.indeterminate = checked.length > 0 && checked.length < boxes.length;
      selectAll.disabled = boxes.length === 0;
    }
    if (deleteBtn) deleteBtn.disabled = checked.length === 0;
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
    const rows = await res.json();
    if (!bucketsCache.length) {
      const bRes = await api()('/api/v1/buckets');
      if (bRes.ok) bucketsCache = await bRes.json();
    }
    const bucketName = (id) => (bucketsCache.find((b) => b.id === id) || {}).name || id;
    const body = document.getElementById('recycle-body');
    body.innerHTML = '';
    const selectAll = document.getElementById('recycle-select-all');
    if (selectAll) {
      selectAll.checked = false;
      selectAll.indeterminate = false;
    }
    for (const o of rows) {
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
    document.getElementById('recycle-empty').hidden = rows.length > 0;
    updateRecycleSelectionUi();
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

  document.getElementById('replication-cancel').addEventListener('click', closeReplicationDialog);
  document.querySelector('[data-replication-close]').addEventListener('click', closeReplicationDialog);
  document.getElementById('replication-all-nodes').addEventListener('change', syncReplicationPeerUi);
  document.getElementById('replication-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const id = Number(document.getElementById('replication-bucket-id').value);
    const replicate_to_all = document.getElementById('replication-all-nodes').checked;
    const peer_ids = Array.from(
      document.querySelectorAll('#replication-peer-checkboxes input:checked'),
    ).map((el) => el.value);
    try {
      const res = await api()(`/api/v1/buckets/${id}/replication`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ replicate_to_all, peer_ids }),
      });
      if (!res.ok && res.status !== 204) throw new Error(await res.text());
      closeReplicationDialog();
      showSettingsStatus('Replication settings saved');
      await loadBuckets();
    } catch (err) {
      showSettingsStatus(String(err.message || err), true);
    }
  });

  window.MyS3Settings = {
    showPanel,
    onAuth(auth) {
      const ownerOnly = ['accounts', 'roles'];
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
