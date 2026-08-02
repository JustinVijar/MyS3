(function () {
  const PAGE_SIZE = 25;

  function clampPage(page, totalPages) {
    const p = Math.max(1, Number(page) || 1);
    return Math.min(p, Math.max(1, totalPages));
  }

  function pageCount(total, pageSize) {
    const size = pageSize || PAGE_SIZE;
    return Math.max(1, Math.ceil(Math.max(0, total) / size));
  }

  function slice(items, page, pageSize) {
    const size = pageSize || PAGE_SIZE;
    const total = (items || []).length;
    const pages = pageCount(total, size);
    const p = clampPage(page, pages);
    const start = (p - 1) * size;
    return {
      items: (items || []).slice(start, start + size),
      page: p,
      pageSize: size,
      total,
      totalPages: pages,
      start: total === 0 ? 0 : start + 1,
      end: Math.min(total, start + size),
    };
  }

  /** Compact page number list with ellipses, e.g. [1, '…', 4, 5, 6, '…', 20] */
  function pageTokens(page, totalPages) {
    if (totalPages <= 7) {
      return Array.from({ length: totalPages }, (_, i) => i + 1);
    }
    const tokens = [];
    const push = (v) => {
      if (tokens[tokens.length - 1] !== v) tokens.push(v);
    };
    push(1);
    if (page > 3) push('…');
    for (let p = Math.max(2, page - 1); p <= Math.min(totalPages - 1, page + 1); p += 1) {
      push(p);
    }
    if (page < totalPages - 2) push('…');
    push(totalPages);
    return tokens;
  }

  /**
   * Render pretty pagination controls into `el`.
   * @param {HTMLElement|null} el
   * @param {{ page: number, total: number, pageSize?: number, onChange: (page: number) => void }} opts
   */
  function render(el, opts) {
    if (!el) return;
    const pageSize = opts.pageSize || PAGE_SIZE;
    const total = Math.max(0, Number(opts.total) || 0);
    const totalPages = pageCount(total, pageSize);
    const page = clampPage(opts.page, totalPages);
    const start = total === 0 ? 0 : (page - 1) * pageSize + 1;
    const end = Math.min(total, page * pageSize);

    el.hidden = total <= pageSize;
    el.classList.add('pager');
    el.setAttribute('role', 'navigation');
    el.setAttribute('aria-label', 'Pagination');

    if (total <= pageSize) {
      el.innerHTML = '';
      return;
    }

    const tokens = pageTokens(page, totalPages);
    const nums = tokens
      .map((t) => {
        if (t === '…') return '<span class="pager-ellipsis" aria-hidden="true">…</span>';
        const active = t === page ? ' is-active' : '';
        return `<button type="button" class="pager-page${active}" data-page="${t}" aria-label="Page ${t}" ${
          t === page ? 'aria-current="page"' : ''
        }>${t}</button>`;
      })
      .join('');

    el.innerHTML = `
      <div class="pager-meta">
        <span class="pager-range">${start}–${end}</span>
        <span class="pager-of">of ${total}</span>
      </div>
      <div class="pager-controls">
        <button type="button" class="pager-nav" data-page="${page - 1}" ${
          page <= 1 ? 'disabled' : ''
        } aria-label="Previous page">
          <span aria-hidden="true">‹</span> Prev
        </button>
        <div class="pager-pages">${nums}</div>
        <button type="button" class="pager-nav" data-page="${page + 1}" ${
          page >= totalPages ? 'disabled' : ''
        } aria-label="Next page">
          Next <span aria-hidden="true">›</span>
        </button>
      </div>`;

    el.querySelectorAll('[data-page]').forEach((btn) => {
      btn.addEventListener('click', () => {
        if (btn.disabled) return;
        const next = Number(btn.getAttribute('data-page'));
        if (!Number.isFinite(next) || next < 1 || next > totalPages || next === page) return;
        if (typeof opts.onChange === 'function') opts.onChange(next);
      });
    });
  }

  window.MyS3Pager = {
    PAGE_SIZE,
    pageCount,
    clampPage,
    slice,
    render,
  };
})();
