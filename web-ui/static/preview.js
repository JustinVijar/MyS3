/**
 * In-app object preview: images, video, text (+ syntax highlight), markdown.
 * Uses window.MyS3.api / getBucket when auth is present; falls back to fetch.
 */
(function () {
  const IMAGE_EXT = new Set([
    'jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp', 'svg', 'svgz', 'ico', 'avif', 'jfif', 'pjpeg', 'pjp',
  ]);
  const VIDEO_EXT = new Set([
    'mp4', 'webm', 'ogg', 'ogv', 'mov', 'm4v', 'mkv',
  ]);
  const MARKDOWN_EXT = new Set(['md', 'markdown', 'mdown', 'mkd']);

  /** Extension → highlight.js language id (null = plain text, no highlight). */
  const LANG_BY_EXT = {
    md: 'markdown',
    markdown: 'markdown',
    mdown: 'markdown',
    mkd: 'markdown',
    txt: null,
    text: null,
    log: null,
    csv: null,
    tsv: null,
    json: 'json',
    jsonc: 'json',
    json5: 'json',
    js: 'javascript',
    mjs: 'javascript',
    cjs: 'javascript',
    jsx: 'javascript',
    ts: 'typescript',
    tsx: 'typescript',
    py: 'python',
    pyw: 'python',
    rs: 'rust',
    go: 'go',
    java: 'java',
    kt: 'kotlin',
    kts: 'kotlin',
    c: 'c',
    h: 'c',
    cpp: 'cpp',
    cc: 'cpp',
    cxx: 'cpp',
    hpp: 'cpp',
    hh: 'cpp',
    cs: 'csharp',
    rb: 'ruby',
    php: 'php',
    swift: 'swift',
    scala: 'scala',
    sh: 'bash',
    bash: 'bash',
    zsh: 'bash',
    fish: 'bash',
    ps1: 'powershell',
    psm1: 'powershell',
    sql: 'sql',
    html: 'xml',
    htm: 'xml',
    xhtml: 'xml',
    xml: 'xml',
    svg: 'xml',
    css: 'css',
    scss: 'scss',
    less: 'less',
    yml: 'yaml',
    yaml: 'yaml',
    toml: 'ini',
    ini: 'ini',
    conf: 'ini',
    cfg: 'ini',
    env: 'bash',
    dockerfile: 'dockerfile',
    makefile: 'makefile',
    mk: 'makefile',
    cmake: 'cmake',
    graphql: 'graphql',
    gql: 'graphql',
    proto: 'protobuf',
    r: 'r',
    lua: 'lua',
    pl: 'perl',
    pm: 'perl',
    vim: 'vim',
    diff: 'diff',
    patch: 'diff',
    tex: 'latex',
    bib: 'latex',
    rst: 'markdown',
    adoc: 'asciidoc',
    properties: 'properties',
    gradle: 'gradle',
    groovy: 'groovy',
    dart: 'dart',
    hs: 'haskell',
    elm: 'elm',
    ex: 'elixir',
    exs: 'elixir',
    erl: 'erlang',
    clj: 'clojure',
    cljs: 'clojure',
    lisp: 'lisp',
    ml: 'ocaml',
    mli: 'ocaml',
    zig: 'rust',
    nim: 'nim',
    v: 'v',
    vue: 'xml',
    svelte: 'xml',
    lock: 'json',
  };

  const TEXT_MAX_BYTES = 2 * 1024 * 1024;

  const modal = document.getElementById('preview-modal');
  const titleEl = document.getElementById('preview-title');
  const bodyEl = document.getElementById('preview-body');
  const mdToggle = document.getElementById('preview-md-toggle');
  const downloadBtn = document.getElementById('preview-download');
  const closeBtn = document.getElementById('preview-close');

  if (!modal || !titleEl || !bodyEl || !mdToggle || !downloadBtn || !closeBtn) {
    console.warn('MyS3 preview: modal markup missing');
    return;
  }

  /** @type {string|null} */
  let currentKey = null;
  /** @type {'preview'|'source'} */
  let mdMode = 'preview';
  /** @type {string|null} */
  let cachedText = null;
  /** @type {string|null} */
  let objectUrl = null;

  function fileName(key) {
    const parts = key.split('/');
    return parts[parts.length - 1] || key;
  }

  function extension(key) {
    const base = fileName(key);
    if (base.toLowerCase() === 'dockerfile' || base.toLowerCase() === 'makefile') {
      return base.toLowerCase();
    }
    const i = base.lastIndexOf('.');
    if (i <= 0) return '';
    return base.slice(i + 1).toLowerCase();
  }

  function encodeKeyPath(key) {
    return key.split('/').map(encodeURIComponent).join('/');
  }

  function contentUrl(key) {
    const bucket =
      window.MyS3 && typeof window.MyS3.getBucket === 'function'
        ? window.MyS3.getBucket()
        : 'storage';
    return (
      '/api/v1/objects/content/' +
      encodeKeyPath(key) +
      '?bucket=' +
      encodeURIComponent(bucket || 'storage')
    );
  }

  function fetchContent(key) {
    const url = contentUrl(key);
    if (window.MyS3 && typeof window.MyS3.api === 'function') {
      return window.MyS3.api(url);
    }
    return fetch(url, { credentials: 'same-origin' });
  }

  function doDownload(key) {
    if (window.MyS3 && typeof window.MyS3.downloadObject === 'function') {
      window.MyS3.downloadObject(key);
      return;
    }
    if (typeof downloadObject === 'function') {
      downloadObject(key);
      return;
    }
    fetchContent(key)
      .then(async (res) => {
        if (!res.ok) throw new Error(await res.text());
        return res.blob();
      })
      .then((blob) => {
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = fileName(key);
        document.body.appendChild(a);
        a.click();
        a.remove();
        URL.revokeObjectURL(url);
      })
      .catch((err) => {
        if (window.MyS3 && typeof window.MyS3.showStatus === 'function') {
          window.MyS3.showStatus(String(err.message || err), true);
        }
      });
  }

  /**
   * @returns {'image'|'video'|'markdown'|'text'|'binary'}
   */
  function detectKind(key, contentType) {
    const ext = extension(key);
    if (IMAGE_EXT.has(ext)) return 'image';
    if (VIDEO_EXT.has(ext)) return 'video';
    if (MARKDOWN_EXT.has(ext)) return 'markdown';
    if (Object.prototype.hasOwnProperty.call(LANG_BY_EXT, ext)) return 'text';

    const ct = (contentType || '').toLowerCase().split(';')[0].trim();
    if (ct.startsWith('image/')) return 'image';
    if (ct.startsWith('video/')) return 'video';
    if (ct === 'text/markdown' || ct === 'text/x-markdown') return 'markdown';
    if (
      ct.startsWith('text/') ||
      ct === 'application/json' ||
      ct === 'application/javascript' ||
      ct === 'application/xml' ||
      ct === 'application/yaml' ||
      ct === 'application/x-yaml' ||
      ct === 'application/toml' ||
      ct.endsWith('+json') ||
      ct.endsWith('+xml')
    ) {
      return 'text';
    }
    return 'binary';
  }

  function languageFor(key) {
    const ext = extension(key);
    if (Object.prototype.hasOwnProperty.call(LANG_BY_EXT, ext)) {
      return LANG_BY_EXT[ext];
    }
    return null;
  }

  function revokeObjectUrl() {
    if (objectUrl) {
      URL.revokeObjectURL(objectUrl);
      objectUrl = null;
    }
  }

  function setMdToggleVisible(visible) {
    mdToggle.hidden = !visible;
    if (visible) {
      mdToggle.querySelectorAll('[data-md-mode]').forEach((btn) => {
        btn.classList.toggle('active', btn.getAttribute('data-md-mode') === mdMode);
      });
    }
  }

  function highlightCode(code, lang) {
    if (typeof hljs === 'undefined') {
      return escHtml(code);
    }
    try {
      if (lang && hljs.getLanguage(lang)) {
        return hljs.highlight(code, { language: lang }).value;
      }
      return hljs.highlightAuto(code).value;
    } catch {
      return escHtml(code);
    }
  }

  function escHtml(s) {
    return String(s)
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;');
  }

  function renderTextBlock(code, lang) {
    const html = highlightCode(code, lang);
    bodyEl.innerHTML = `<pre class="preview-code"><code class="hljs${lang ? ' language-' + escHtml(lang) : ''}">${html}</code></pre>`;
  }

  function renderMarkdown() {
    if (mdMode === 'source') {
      renderTextBlock(cachedText || '', 'markdown');
      return;
    }
    let html = cachedText || '';
    if (typeof marked !== 'undefined') {
      const renderer = new marked.Renderer();
      renderer.code = function ({ text, lang }) {
        const language = (lang || '').trim().split(/\s+/)[0] || null;
        const highlighted = highlightCode(text, language);
        const cls = language
          ? `hljs language-${escHtml(language)}`
          : 'hljs';
        return `<pre><code class="${cls}">${highlighted}</code></pre>\n`;
      };
      html = marked.parse(cachedText || '', { gfm: true, breaks: false, renderer });
    } else {
      html = `<pre>${escHtml(cachedText || '')}</pre>`;
    }
    if (typeof DOMPurify !== 'undefined') {
      html = DOMPurify.sanitize(html, { USE_PROFILES: { html: true } });
    }
    bodyEl.innerHTML = `<div class="preview-md">${html}</div>`;
    bodyEl.querySelectorAll('pre code').forEach((block) => {
      if (typeof hljs !== 'undefined' && !block.classList.contains('hljs')) {
        hljs.highlightElement(block);
      }
    });
  }

  function showBinaryFallback(key) {
    bodyEl.innerHTML = `
      <div class="preview-fallback">
        <p>This file type can’t be previewed in the browser.</p>
        <button type="button" class="btn primary" id="preview-fallback-dl">Download ${escHtml(fileName(key))}</button>
      </div>`;
    bodyEl.querySelector('#preview-fallback-dl').addEventListener('click', () => doDownload(key));
  }

  async function loadText(key) {
    const res = await fetchContent(key);
    if (!res.ok) throw new Error(await res.text() || res.statusText);
    const len = Number(res.headers.get('content-length') || 0);
    if (len > TEXT_MAX_BYTES) {
      throw new Error(`File is too large to preview (${formatBytesLocal(len)}). Download instead.`);
    }
    const buf = await res.arrayBuffer();
    if (buf.byteLength > TEXT_MAX_BYTES) {
      throw new Error(`File is too large to preview (${formatBytesLocal(buf.byteLength)}). Download instead.`);
    }
    const ct = res.headers.get('content-type') || '';
    const text = new TextDecoder('utf-8', { fatal: false }).decode(buf);
    return { text, contentType: ct };
  }

  async function loadMediaUrl(key) {
    const res = await fetchContent(key);
    if (!res.ok) throw new Error(await res.text() || res.statusText);
    const blob = await res.blob();
    revokeObjectUrl();
    objectUrl = URL.createObjectURL(blob);
    return objectUrl;
  }

  function formatBytesLocal(n) {
    if (window.MyS3 && typeof window.MyS3.formatBytes === 'function') {
      return window.MyS3.formatBytes(n);
    }
    if (typeof formatBytes === 'function') return formatBytes(n);
    if (n < 1024) return n + ' B';
    if (n < 1024 ** 2) return (n / 1024).toFixed(1) + ' KiB';
    return (n / 1024 ** 2).toFixed(1) + ' MiB';
  }

  /**
   * @param {string} key
   */
  async function openPreview(key) {
    currentKey = key;
    cachedText = null;
    mdMode = 'preview';
    revokeObjectUrl();
    titleEl.textContent = fileName(key);
    titleEl.title = key;
    bodyEl.innerHTML = `<div class="preview-loading">Loading…</div>`;
    setMdToggleVisible(false);
    modal.hidden = false;
    document.body.classList.add('preview-open');
    closeBtn.focus();

    const kindHint = detectKind(key, '');

    try {
      if (kindHint === 'image') {
        setMdToggleVisible(false);
        const url = await loadMediaUrl(key);
        bodyEl.innerHTML = `<div class="preview-media"><img alt="${escHtml(fileName(key))}" src="${escHtml(url)}" /></div>`;
        return;
      }

      if (kindHint === 'video') {
        setMdToggleVisible(false);
        const url = await loadMediaUrl(key);
        bodyEl.innerHTML = `
          <div class="preview-media">
            <video controls playsinline src="${escHtml(url)}"></video>
          </div>`;
        return;
      }

      if (kindHint === 'markdown' || kindHint === 'text') {
        const { text, contentType } = await loadText(key);
        const kind = detectKind(key, contentType);
        cachedText = text;
        if (kind === 'markdown' || kindHint === 'markdown') {
          setMdToggleVisible(true);
          renderMarkdown();
        } else {
          setMdToggleVisible(false);
          renderTextBlock(text, languageFor(key));
        }
        return;
      }

      showBinaryFallback(key);
    } catch (err) {
      bodyEl.innerHTML = `<div class="preview-fallback"><p class="preview-error">${escHtml(String(err.message || err))}</p></div>`;
      if (typeof showStatus === 'function') {
        showStatus(String(err.message || err), true);
      } else if (window.MyS3 && typeof window.MyS3.showStatus === 'function') {
        window.MyS3.showStatus(String(err.message || err), true);
      }
    }
  }

  function closePreview() {
    modal.hidden = true;
    document.body.classList.remove('preview-open');
    currentKey = null;
    cachedText = null;
    bodyEl.innerHTML = '';
    setMdToggleVisible(false);
    revokeObjectUrl();
  }

  function isPreviewable(key) {
    return detectKind(key, '') !== 'binary';
  }

  closeBtn.addEventListener('click', closePreview);
  modal.querySelectorAll('[data-preview-close]').forEach((el) => {
    el.addEventListener('click', closePreview);
  });
  downloadBtn.addEventListener('click', () => {
    if (currentKey) doDownload(currentKey);
  });
  mdToggle.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-md-mode]');
    if (!btn || !currentKey) return;
    mdMode = btn.getAttribute('data-md-mode') === 'source' ? 'source' : 'preview';
    setMdToggleVisible(true);
    renderMarkdown();
  });

  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !modal.hidden) {
      e.preventDefault();
      closePreview();
    }
  });

  window.openPreview = openPreview;
  window.closePreview = closePreview;
  window.isPreviewable = isPreviewable;
})();
