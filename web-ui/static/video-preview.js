/**
 * Client-side video preview: stream content → OPFS → ffmpeg.wasm (time slices) → MediaSource.
 * Exposes window.MyS3VideoPreview for preview.js / share.js.
 */
(function () {
  const CORE_BASE = '/static/vendor/ffmpeg/core/';
  const SLICE_SEC = 4;
  const BUFFER_AHEAD_SEC = 16;
  const BUFFER_BEHIND_SEC = 8;
  const MIME_CODEC = 'video/mp4; codecs="avc1.42E01E,mp4a.40.2"';
  const OPFS_DIR = 'mys3-preview';
  /** Browser can progressive-play these with Range; skip full OPFS + wasm re-encode. */
  const NATIVE_VIDEO_EXT = new Set(['mp4', 'webm', 'ogg', 'ogv', 'm4v', 'mov']);

  /** @type {{ FFmpeg: typeof import('/static/vendor/ffmpeg/ffmpeg/index.js').FFmpeg, toBlobURL: Function } | null} */
  let libs = null;
  /** @type {import('/static/vendor/ffmpeg/ffmpeg/index.js').FFmpeg | null} */
  let ffmpeg = null;
  /** @type {Promise<import('/static/vendor/ffmpeg/ffmpeg/index.js').FFmpeg> | null} */
  let ffmpegLoadPromise = null;
  /** @type {PreviewSession | NativeSession | null} */
  let activeSession = null;

  function fileExt(key) {
    const base = String(key || '').split('/').pop() || '';
    const i = base.lastIndexOf('.');
    if (i <= 0) return '';
    return base.slice(i + 1).toLowerCase();
  }

  function isNativePlayable(key) {
    return NATIVE_VIDEO_EXT.has(fileExt(key));
  }

  function mediaSourceCtor() {
    return window.ManagedMediaSource || window.MediaSource || null;
  }

  function canTranscode() {
    const MS = mediaSourceCtor();
    if (!MS) return false;
    if (typeof MS.isTypeSupported === 'function' && !MS.isTypeSupported(MIME_CODEC)) {
      return false;
    }
    return !!(navigator.storage && typeof navigator.storage.getDirectory === 'function');
  }

  function isSupported() {
    return typeof HTMLVideoElement !== 'undefined';
  }

  async function loadLibs() {
    if (libs) return libs;
    const [ffmpegMod, utilMod] = await Promise.all([
      import('/static/vendor/ffmpeg/ffmpeg/index.js'),
      import('/static/vendor/ffmpeg/util/index.js'),
    ]);
    libs = { FFmpeg: ffmpegMod.FFmpeg, toBlobURL: utilMod.toBlobURL };
    return libs;
  }

  async function ensureFfmpeg(signal) {
    if (ffmpeg && ffmpeg.loaded) return ffmpeg;
    if (ffmpegLoadPromise) return ffmpegLoadPromise;
    ffmpegLoadPromise = (async () => {
      const { FFmpeg, toBlobURL } = await loadLibs();
      if (signal && signal.aborted) throw new DOMException('Aborted', 'AbortError');
      const instance = new FFmpeg();
      const coreURL = await toBlobURL(CORE_BASE + 'ffmpeg-core.js', 'text/javascript');
      const wasmURL = await toBlobURL(CORE_BASE + 'ffmpeg-core.wasm', 'application/wasm');
      await instance.load({ coreURL, wasmURL }, { signal });
      ffmpeg = instance;
      return instance;
    })().finally(() => {
      ffmpegLoadPromise = null;
    });
    return ffmpegLoadPromise;
  }

  /**
   * Stream a fetch Response body into OPFS; return a File for WORKERFS mounting.
   * @param {Response} response
   * @param {AbortSignal|undefined} signal
   * @param {(msg: string) => void} [onStatus]
   */
  async function stageToOpfs(response, signal, onStatus) {
    if (!response.ok) {
      const text = await response.text().catch(() => '');
      throw new Error(text || response.statusText || 'Failed to download video');
    }
    if (!response.body) throw new Error('Response body is not readable');

    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle(OPFS_DIR, { create: true });
    const name = 'in-' + Date.now() + '-' + Math.random().toString(36).slice(2, 10) + '.bin';
    const handle = await dir.getFileHandle(name, { create: true });
    const writable = await handle.createWritable();

    const total = Number(response.headers.get('content-length') || 0);
    let received = 0;
    const reader = response.body.getReader();

    try {
      if (onStatus) onStatus('Downloading…');
      for (;;) {
        if (signal && signal.aborted) throw new DOMException('Aborted', 'AbortError');
        const { done, value } = await reader.read();
        if (done) break;
        if (value && value.byteLength) {
          await writable.write(value);
          received += value.byteLength;
          if (onStatus && total > 0) {
            const pct = Math.min(99, Math.floor((received / total) * 100));
            onStatus('Downloading… ' + pct + '%');
          }
        }
      }
      await writable.close();
    } catch (err) {
      try {
        await writable.abort();
      } catch (_) {
        /* ignore */
      }
      try {
        await dir.removeEntry(name);
      } catch (_) {
        /* ignore */
      }
      throw err;
    }

    const file = await handle.getFile();
    return {
      file,
      name,
      dir,
      cleanup: async () => {
        try {
          await dir.removeEntry(name);
        } catch (_) {
          /* ignore */
        }
      },
    };
  }

  function parseHeight(height) {
    if (height == null || height === '' || height === 'original' || height === '0') {
      return null;
    }
    const n = Number(height);
    if (!Number.isFinite(n) || n <= 0) return null;
    return Math.floor(n);
  }

  function buildSliceArgs(inputPath, outName, startSec, heightPx) {
    const args = ['-ss', String(startSec), '-i', inputPath, '-t', String(SLICE_SEC)];
    if (heightPx) {
      args.push('-vf', "scale=-2:'min(ih," + heightPx + ")'");
    }
    args.push(
      '-c:v',
      'libx264',
      '-preset',
      'veryfast',
      '-c:a',
      'aac',
      '-ac',
      '2',
      '-movflags',
      'frag_keyframe+empty_moov+default_base_moof',
      '-f',
      'mp4',
      outName
    );
    return args;
  }

  async function probeDuration(ff, inputPath, signal) {
    let fromLog = 0;
    const onLog = ({ message }) => {
      const m = /Duration:\s*(\d+):(\d+):(\d+(?:\.\d+)?)/.exec(message || '');
      if (m) {
        fromLog =
          parseInt(m[1], 10) * 3600 + parseInt(m[2], 10) * 60 + parseFloat(m[3]);
      }
    };
    ff.on('log', onLog);
    try {
      await ff.exec(['-hide_banner', '-i', inputPath], 15000, { signal }).catch(() => 1);
    } finally {
      ff.off('log', onLog);
    }
    if (fromLog > 0 && Number.isFinite(fromLog)) return fromLog;

    try {
      await ff.ffprobe(
        [
          '-v',
          'error',
          '-show_entries',
          'format=duration',
          '-of',
          'default=noprint_wrappers=1:nokey=1',
          inputPath,
          '-o',
          'duration.txt',
        ],
        15000,
        { signal }
      );
      const text = await ff.readFile('duration.txt', 'utf8');
      await ff.deleteFile('duration.txt').catch(() => {});
      const n = parseFloat(String(text).trim());
      if (Number.isFinite(n) && n > 0) return n;
    } catch (_) {
      /* fall through */
    }
    return 0;
  }

  function forwardBufferedEnd(video) {
    const b = video.buffered;
    if (!b || b.length === 0) return 0;
    let end = 0;
    for (let i = 0; i < b.length; i++) {
      if (b.start(i) <= video.currentTime + 0.5) {
        end = Math.max(end, b.end(i));
      }
    }
    if (end === 0 && b.length) end = b.end(b.length - 1);
    return end;
  }

  function waitFor(predicate, signal, timeoutMs) {
    return new Promise((resolve, reject) => {
      const start = Date.now();
      const tick = () => {
        if (signal && signal.aborted) {
          reject(new DOMException('Aborted', 'AbortError'));
          return;
        }
        if (predicate()) {
          resolve();
          return;
        }
        if (timeoutMs != null && Date.now() - start > timeoutMs) {
          reject(new Error('Timed out waiting for MediaSource'));
          return;
        }
        setTimeout(tick, 40);
      };
      tick();
    });
  }

  class PreviewSession {
    /**
     * @param {object} opts
     * @param {HTMLVideoElement} opts.video
     * @param {() => Promise<Response>} opts.fetchContent
     * @param {string} [opts.height]
     * @param {AbortSignal} [opts.signal]
     * @param {(msg: string) => void} [opts.onStatus]
     */
    constructor(opts) {
      this.video = opts.video;
      this.fetchContent = opts.fetchContent;
      this.height = opts.height;
      this.signal = opts.signal;
      this.onStatus = opts.onStatus || (() => {});
      this._destroyed = false;
      this._opfs = null;
      this._mediaSource = null;
      this._sourceBuffer = null;
      this._objectUrl = null;
      this._mounted = false;
      this._inputPath = '';
      this._duration = 0;
      this._nextStart = 0;
      this._producing = false;
      this._appendChain = Promise.resolve();
      this._seekHandler = null;
      this._timeHandler = null;
      this._generation = 0;
    }

    async start() {
      if (!canTranscode()) {
        throw new Error('Browser does not support MediaSource + OPFS video preview');
      }
      this.onStatus('Loading transcoder…');
      const ff = await ensureFfmpeg(this.signal);
      this._throwIfAborted();

      this.onStatus('Downloading…');
      const res = await this.fetchContent();
      this._throwIfAborted();
      this._opfs = await stageToOpfs(res, this.signal, this.onStatus);
      this._throwIfAborted();

      this.onStatus('Preparing…');
      await ff.createDir('/input').catch(() => {});
      await ff.mount('WORKERFS', { files: [this._opfs.file] }, '/input');
      this._mounted = true;
      this._inputPath = '/input/' + this._opfs.file.name;

      this._duration = await probeDuration(ff, this._inputPath, this.signal);
      this._throwIfAborted();

      await this._attachMediaSource();
      this._throwIfAborted();

      this._seekHandler = () => this._onSeek();
      this._timeHandler = () => {
        this._trimBuffer();
        this._kickProduce();
      };
      this.video.addEventListener('seeking', this._seekHandler);
      this.video.addEventListener('timeupdate', this._timeHandler);

      this.onStatus('Starting preview…');
      // Await the first slice so callers see encode failures; further slices run in background.
      await this._produceNextSlice();
      this._kickProduce();
    }

    async _attachMediaSource() {
      const MS = mediaSourceCtor();
      if (this._objectUrl) {
        try {
          URL.revokeObjectURL(this._objectUrl);
        } catch (_) {
          /* ignore */
        }
        this._objectUrl = null;
      }
      const mediaSource = new MS();
      this._mediaSource = mediaSource;
      if (window.ManagedMediaSource && mediaSource instanceof ManagedMediaSource) {
        this.video.disableRemotePlayback = true;
      }
      this._objectUrl = URL.createObjectURL(mediaSource);
      this.video.src = this._objectUrl;

      await new Promise((resolve, reject) => {
        const onOpen = () => {
          cleanup();
          resolve();
        };
        const onErr = () => {
          cleanup();
          reject(new Error('MediaSource failed to open'));
        };
        const onAbort = () => {
          cleanup();
          reject(new DOMException('Aborted', 'AbortError'));
        };
        const cleanup = () => {
          mediaSource.removeEventListener('sourceopen', onOpen);
          mediaSource.removeEventListener('error', onErr);
          if (this.signal) this.signal.removeEventListener('abort', onAbort);
        };
        mediaSource.addEventListener('sourceopen', onOpen, { once: true });
        mediaSource.addEventListener('error', onErr, { once: true });
        if (this.signal) {
          if (this.signal.aborted) {
            onAbort();
            return;
          }
          this.signal.addEventListener('abort', onAbort, { once: true });
        }
      });

      if (!mediaSource.addSourceBuffer) {
        throw new Error('MediaSource has no addSourceBuffer');
      }
      const sb = mediaSource.addSourceBuffer(MIME_CODEC);
      try {
        sb.mode = 'sequence';
      } catch (_) {
        /* some browsers reject mode changes */
      }
      this._sourceBuffer = sb;
      if (this._duration > 0 && typeof mediaSource.duration === 'number') {
        try {
          mediaSource.duration = this._duration;
        } catch (_) {
          /* ignore */
        }
      }
    }

    _throwIfAborted() {
      if (this._destroyed || (this.signal && this.signal.aborted)) {
        throw new DOMException('Aborted', 'AbortError');
      }
    }

    _kickProduce() {
      if (this._producing || this._destroyed) return;
      this._producing = true;
      this._produceLoop()
        .catch((err) => {
          if (err && err.name === 'AbortError') return;
          if (!this._destroyed) {
            console.error('MyS3 video preview:', err);
            this.onStatus(String((err && err.message) || err));
          }
        })
        .finally(() => {
          this._producing = false;
        });
    }

    /**
     * Encode and append a single slice at `_nextStart`.
     * @returns {'ok'|'done'|'wait'}
     */
    async _produceNextSlice() {
      const gen = this._generation;
      const ff = ffmpeg;
      if (!ff || !ff.loaded) throw new Error('ffmpeg not loaded');
      this._throwIfAborted();

      if (this._duration > 0 && this._nextStart >= this._duration - 0.05) {
        await this._endStream();
        this.onStatus('');
        return 'done';
      }

      const ahead = forwardBufferedEnd(this.video) - this.video.currentTime;
      if (ahead >= BUFFER_AHEAD_SEC && this.video.buffered.length > 0) {
        this.onStatus('');
        return 'wait';
      }

      const start = this._nextStart;
      this.onStatus(
        this._duration > 0
          ? 'Buffering… ' + Math.min(100, Math.floor((start / this._duration) * 100)) + '%'
          : 'Buffering…'
      );

      const outName = 'slice-' + Math.floor(start * 1000) + '.mp4';
      const heightPx = parseHeight(this.height);
      const args = buildSliceArgs(this._inputPath, outName, start, heightPx);
      let code = await ff.exec(args, -1, { signal: this.signal });
      if (this._destroyed || this._generation !== gen) {
        await ff.deleteFile(outName).catch(() => {});
        return 'done';
      }

      if (code !== 0) {
        const argsNoAudio = ['-ss', String(start), '-i', this._inputPath, '-t', String(SLICE_SEC)];
        if (heightPx) {
          argsNoAudio.push('-vf', "scale=-2:'min(ih," + heightPx + ")'");
        }
        argsNoAudio.push(
          '-c:v',
          'libx264',
          '-preset',
          'veryfast',
          '-an',
          '-movflags',
          'frag_keyframe+empty_moov+default_base_moof',
          '-f',
          'mp4',
          outName
        );
        code = await ff.exec(argsNoAudio, -1, { signal: this.signal });
      }

      if (code !== 0) {
        if (start === 0) {
          throw new Error('ffmpeg failed to transcode this video in the browser');
        }
        await this._endStream();
        this.onStatus('');
        return 'done';
      }

      let data;
      try {
        data = await ff.readFile(outName);
      } finally {
        await ff.deleteFile(outName).catch(() => {});
      }

      if (!data || !data.byteLength) {
        if (start === 0) throw new Error('Transcode produced empty output');
        await this._endStream();
        this.onStatus('');
        return 'done';
      }

      await this._appendBuffer(data);
      if (this._destroyed || this._generation !== gen) return 'done';

      this._nextStart = start + SLICE_SEC;

      if (this.video.paused && this.video.readyState >= 2) {
        this.video.play().catch(() => {});
      }
      this._trimBuffer();
      return 'ok';
    }

    async _produceLoop() {
      const gen = this._generation;
      while (!this._destroyed && this._generation === gen) {
        this._throwIfAborted();
        const result = await this._produceNextSlice();
        if (result === 'done') return;
        if (result === 'wait') {
          await waitFor(
            () =>
              this._destroyed ||
              this._generation !== gen ||
              forwardBufferedEnd(this.video) - this.video.currentTime < BUFFER_AHEAD_SEC * 0.6,
            this.signal,
            null
          );
        }
      }
    }

    _appendBuffer(uint8) {
      const sb = this._sourceBuffer;
      const ms = this._mediaSource;
      if (!sb || !ms || ms.readyState !== 'open') {
        return Promise.resolve();
      }
      const chunk =
        uint8 instanceof Uint8Array
          ? uint8
          : new Uint8Array(uint8.buffer || uint8);

      this._appendChain = this._appendChain.then(
        () =>
          new Promise((resolve, reject) => {
            if (this._destroyed || !this._sourceBuffer || this._mediaSource.readyState !== 'open') {
              resolve();
              return;
            }
            const onUpdate = () => {
              cleanup();
              resolve();
            };
            const onError = () => {
              cleanup();
              reject(new Error('SourceBuffer append failed'));
            };
            const cleanup = () => {
              sb.removeEventListener('updateend', onUpdate);
              sb.removeEventListener('error', onError);
            };
            const tryAppend = () => {
              if (this._destroyed) {
                cleanup();
                resolve();
                return;
              }
              if (sb.updating) {
                sb.addEventListener('updateend', tryAppend, { once: true });
                return;
              }
              sb.addEventListener('updateend', onUpdate, { once: true });
              sb.addEventListener('error', onError, { once: true });
              try {
                sb.appendBuffer(chunk);
              } catch (err) {
                cleanup();
                reject(err);
              }
            };
            tryAppend();
          })
      );
      return this._appendChain;
    }

    async _trimBuffer() {
      const sb = this._sourceBuffer;
      const video = this.video;
      if (!sb || sb.updating || !video.buffered.length) return;
      const keepFrom = Math.max(0, video.currentTime - BUFFER_BEHIND_SEC);
      const start0 = video.buffered.start(0);
      if (start0 >= keepFrom - 0.25) return;
      try {
        await new Promise((resolve) => {
          const done = () => {
            sb.removeEventListener('updateend', done);
            resolve();
          };
          if (sb.updating) {
            sb.addEventListener('updateend', done, { once: true });
            return;
          }
          sb.addEventListener('updateend', done, { once: true });
          try {
            sb.remove(start0, keepFrom);
          } catch (_) {
            sb.removeEventListener('updateend', done);
            resolve();
          }
        });
      } catch (_) {
        /* ignore */
      }
    }

    async _endStream() {
      const ms = this._mediaSource;
      const sb = this._sourceBuffer;
      if (!ms || ms.readyState !== 'open') return;
      try {
        await this._appendChain.catch(() => {});
        await waitFor(() => !sb || !sb.updating, this.signal, 10000);
        if (ms.readyState === 'open') ms.endOfStream();
      } catch (_) {
        /* ignore */
      }
    }

    async _onSeek() {
      if (this._destroyed) return;
      const t = this.video.currentTime;
      const buffered = this.video.buffered;
      for (let i = 0; i < buffered.length; i++) {
        if (t >= buffered.start(i) && t <= buffered.end(i) + 0.15) {
          this._kickProduce();
          return;
        }
      }
      // Seek outside buffered range: restart slice pipeline from target.
      this._generation += 1;
      this._nextStart = Math.max(0, Math.floor(t / SLICE_SEC) * SLICE_SEC);
      this._appendChain = Promise.resolve();
      try {
        const sb = this._sourceBuffer;
        const ms = this._mediaSource;
        if (sb && ms && ms.readyState === 'open') {
          await waitFor(() => !sb.updating, this.signal, 5000).catch(() => {});
          if (sb.buffered.length) {
            const from = sb.buffered.start(0);
            const to = sb.buffered.end(sb.buffered.length - 1);
            await new Promise((resolve) => {
              const done = () => {
                sb.removeEventListener('updateend', done);
                resolve();
              };
              sb.addEventListener('updateend', done, { once: true });
              try {
                sb.remove(from, to);
              } catch (_) {
                sb.removeEventListener('updateend', done);
                resolve();
              }
            });
          }
          try {
            sb.timestampOffset = this._nextStart;
          } catch (_) {
            /* ignore */
          }
        }
      } catch (_) {
        /* ignore */
      }
      this._kickProduce();
    }

    async destroy() {
      this._destroyed = true;
      this._generation += 1;
      if (this._seekHandler) {
        this.video.removeEventListener('seeking', this._seekHandler);
        this._seekHandler = null;
      }
      if (this._timeHandler) {
        this.video.removeEventListener('timeupdate', this._timeHandler);
        this._timeHandler = null;
      }
      try {
        this.video.pause();
      } catch (_) {
        /* ignore */
      }
      this.video.removeAttribute('src');
      try {
        this.video.load();
      } catch (_) {
        /* ignore */
      }
      if (this._objectUrl) {
        try {
          URL.revokeObjectURL(this._objectUrl);
        } catch (_) {
          /* ignore */
        }
        this._objectUrl = null;
      }
      this._sourceBuffer = null;
      this._mediaSource = null;

      if (ffmpeg && ffmpeg.loaded && this._mounted) {
        try {
          await ffmpeg.unmount('/input');
        } catch (_) {
          /* ignore */
        }
        this._mounted = false;
      }
      if (this._opfs) {
        await this._opfs.cleanup();
        this._opfs = null;
      }
    }
  }

  /**
   * Progressive native playback via signed/accessible URL (browser Range buffer).
   * Avoids downloading the whole object into OPFS before play.
   */
  class NativeSession {
    /**
     * @param {object} opts
     * @param {HTMLVideoElement} opts.video
     * @param {string} opts.nativeUrl
     * @param {AbortSignal} [opts.signal]
     * @param {(msg: string) => void} [opts.onStatus]
     */
    constructor(opts) {
      this.video = opts.video;
      this.nativeUrl = opts.nativeUrl;
      this.signal = opts.signal;
      this.onStatus = opts.onStatus || (() => {});
      this._destroyed = false;
    }

    async start() {
      if (this.signal && this.signal.aborted) {
        throw new DOMException('Aborted', 'AbortError');
      }
      this.onStatus('Starting preview…');
      this.video.preload = 'auto';
      this.video.src = this.nativeUrl;
      try {
        await this.video.play();
      } catch (_) {
        /* autoplay may be blocked; controls still work */
      }
      this.onStatus('');
    }

    async destroy() {
      this._destroyed = true;
      try {
        this.video.pause();
      } catch (_) {
        /* ignore */
      }
      this.video.removeAttribute('src');
      try {
        this.video.load();
      } catch (_) {
        /* ignore */
      }
    }
  }

  /**
   * @param {object} opts
   * @param {HTMLVideoElement} opts.video
   * @param {() => Promise<Response>} opts.fetchContent
   * @param {string} [opts.nativeUrl] signed/share URL for progressive <video src>
   * @param {string} [opts.key] object key (for native-format detection)
   * @param {boolean} [opts.forceTranscode] skip native path
   * @param {string} [opts.height]
   * @param {AbortSignal} [opts.signal]
   * @param {(msg: string) => void} [opts.onStatus]
   */
  async function play(opts) {
    await destroyActive();
    const useNative =
      !opts.forceTranscode &&
      opts.nativeUrl &&
      (!opts.key || isNativePlayable(opts.key));
    const session = useNative
      ? new NativeSession(opts)
      : new PreviewSession(opts);
    activeSession = session;
    try {
      await session.start();
    } catch (err) {
      if (activeSession === session) {
        await session.destroy();
        activeSession = null;
      }
      throw err;
    }
    return session;
  }

  async function destroyActive() {
    if (!activeSession) return;
    const s = activeSession;
    activeSession = null;
    await s.destroy();
  }

  window.MyS3VideoPreview = {
    isSupported,
    canTranscode,
    isNativePlayable,
    play,
    destroy: destroyActive,
  };
})();
