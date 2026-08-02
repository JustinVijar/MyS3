(function () {
  const modal = document.getElementById('ui-modal');
  const titleEl = document.getElementById('ui-modal-title');
  const bodyEl = document.getElementById('ui-modal-body');
  const actionsEl = document.getElementById('ui-modal-actions');
  const backdrop = modal.querySelector('[data-ui-modal-close]');

  /** @type {null | ((value: unknown) => void)} */
  let resolveOpen = null;
  /** @type {'alert' | 'confirm' | 'prompt' | 'custom' | null} */
  let mode = null;

  function close(result) {
    if (!resolveOpen) return;
    const resolve = resolveOpen;
    resolveOpen = null;
    mode = null;
    modal.hidden = true;
    document.body.classList.remove('ui-modal-open');
    resolve(result);
  }

  function open(opts) {
    return new Promise((resolve) => {
      if (resolveOpen) close(null);
      resolveOpen = resolve;
      mode = opts.mode || 'custom';
      titleEl.textContent = opts.title || '';
      titleEl.hidden = !opts.title;
      bodyEl.innerHTML = '';
      actionsEl.innerHTML = '';

      if (typeof opts.renderBody === 'function') {
        opts.renderBody(bodyEl);
      } else if (opts.message) {
        const p = document.createElement('p');
        p.className = 'ui-modal-message';
        p.textContent = opts.message;
        bodyEl.appendChild(p);
      }

      const buttons = opts.buttons || [{ label: 'OK', value: true, primary: true }];
      for (const btn of buttons) {
        const el = document.createElement('button');
        el.type = 'button';
        el.className =
          (btn.primary ? 'btn primary' : 'btn ghost') + (btn.danger ? ' danger' : '');
        el.textContent = btn.label;
        if (btn.disabled) el.disabled = true;
        el.addEventListener('click', () => {
          if (el.disabled) return;
          const value =
            typeof btn.getValue === 'function' ? btn.getValue() : btn.value;
          close(value);
        });
        actionsEl.appendChild(el);
        if (typeof btn.onCreated === 'function') btn.onCreated(el);
      }

      modal.hidden = false;
      document.body.classList.add('ui-modal-open');
      const focusable = modal.querySelector('input, button.primary, button');
      if (focusable) focusable.focus();
    });
  }

  backdrop.addEventListener('click', () => {
    if (mode === 'alert') close(true);
    else close(mode === 'confirm' ? false : null);
  });

  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || modal.hidden) return;
    e.preventDefault();
    if (mode === 'alert') close(true);
    else close(mode === 'confirm' ? false : null);
  });

  function alert(message, title) {
    return open({
      mode: 'alert',
      title: title || 'Notice',
      message: String(message),
      buttons: [{ label: 'OK', value: true, primary: true }],
    });
  }

  function confirm(message, title) {
    return open({
      mode: 'confirm',
      title: title || 'Confirm',
      message: String(message),
      buttons: [
        { label: 'Cancel', value: false },
        { label: 'Confirm', value: true, primary: true },
      ],
    });
  }

  function prompt(message, defaultValue, title) {
    /** @type {HTMLInputElement | null} */
    let input = null;
    return open({
      mode: 'prompt',
      title: title || 'Input',
      renderBody(body) {
        const p = document.createElement('p');
        p.className = 'ui-modal-message';
        p.textContent = String(message);
        body.appendChild(p);
        input = document.createElement('input');
        input.type = 'text';
        input.className = 'ui-modal-input';
        input.value = defaultValue || '';
        input.addEventListener('keydown', (e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            close(input.value);
          }
        });
        body.appendChild(input);
      },
      buttons: [
        { label: 'Cancel', value: null },
        {
          label: 'OK',
          primary: true,
          getValue: () => (input ? input.value : ''),
        },
      ],
    });
  }

  /**
   * Require the user to type the word "delete" before confirming.
   * @returns {Promise<boolean>}
   */
  function confirmTypeDelete(message, title) {
    /** @type {HTMLInputElement | null} */
    let input = null;
    /** @type {HTMLButtonElement | null} */
    let okBtn = null;
    const sync = () => {
      if (okBtn) okBtn.disabled = !(input && input.value === 'delete');
    };
    return open({
      mode: 'confirm',
      title: title || 'Confirm permanent delete',
      renderBody(body) {
        const p = document.createElement('p');
        p.className = 'ui-modal-message';
        p.textContent = String(message);
        body.appendChild(p);
        const hint = document.createElement('p');
        hint.className = 'ui-modal-message';
        hint.innerHTML = 'Type <strong>delete</strong> to confirm:';
        body.appendChild(hint);
        input = document.createElement('input');
        input.type = 'text';
        input.className = 'ui-modal-input';
        input.autocomplete = 'off';
        input.spellcheck = false;
        input.placeholder = 'delete';
        input.addEventListener('input', sync);
        input.addEventListener('keydown', (e) => {
          if (e.key === 'Enter' && input.value === 'delete') {
            e.preventDefault();
            close(true);
          }
        });
        body.appendChild(input);
      },
      buttons: [
        { label: 'Cancel', value: false },
        {
          label: 'OK',
          value: true,
          primary: true,
          danger: true,
          disabled: true,
          onCreated: (el) => {
            okBtn = el;
            sync();
          },
        },
      ],
    }).then((v) => !!v);
  }

  function showCredentials(username, password, title) {
    return open({
      mode: 'alert',
      title: title || 'Credentials (shown once)',
      renderBody(body) {
        const lead = document.createElement('p');
        lead.className = 'ui-modal-message';
        lead.textContent = 'Save these credentials now — they won’t be shown again.';
        body.appendChild(lead);

        function row(label, value, id) {
          const wrap = document.createElement('label');
          wrap.className = 'field';
          wrap.textContent = label;
          const copyRow = document.createElement('div');
          copyRow.className = 'copy-row';
          const code = document.createElement('code');
          code.id = id;
          code.textContent = value;
          const btn = document.createElement('button');
          btn.type = 'button';
          btn.className = 'btn ghost';
          btn.textContent = 'Copy';
          btn.addEventListener('click', async () => {
            try {
              await navigator.clipboard.writeText(value);
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
        }

        row('Username', username, 'ui-cred-user');
        row('Password', password, 'ui-cred-pass');
      },
      buttons: [{ label: 'Done', value: true, primary: true }],
    });
  }

  window.MyS3UI = {
    alert,
    confirm,
    confirmTypeDelete,
    prompt,
    showCredentials,
    open,
    close,
  };
})();
