// EXTRACTED VERBATIM from Sources/Olaz/ZaloRuntime/OfficialClientHost.swift (AsarSchemeHandler.workerPreludeBase; /*DEBUG_EVAL*/ is replaced by dal-worker-debug-eval.js in debug builds only) — do not edit here; re-run tools/sync-from-macos.sh
(() => {
  const fwd = (k, v) => { try { self.postMessage({ __olazLog: k + ' ' + String(v).slice(0, 300) }); } catch {} };
  self.__olazFwd = fwd;
  /*DEBUG_EVAL*/
  const errText = (e) => e ? (e.name || '') + ': ' + (e.message || e) : String(e);
  self.addEventListener('error', (e) => fwd('error', e.message));
  self.addEventListener('unhandledrejection', (e) => fwd('unhandledrejection', errText(e.reason)));
  for (const lvl of ['warn', 'error']) { const o = console[lvl]; console[lvl] = (...a) => { fwd('console.' + lvl, a.map((x) => x instanceof Error ? errText(x) : (typeof x === 'string' ? x : '')).join(' ')); return o.apply(console, a); }; }
  // The sync-v2 sub-worker writes imported messages through its "lite IDB" path, whose
  // encryption-state store needs the DB client (`database-manager.createQueryBuilder`).
  // Zalo's own SQLite-migrated clients never take this path and nothing in a web worker
  // creates the client (Application lifecycle is unsupported there), so create it once after
  // the sub-worker script has loaded — through its official singleton accessor.
  const ensureDatabaseClient = () => {
    try {
      const req = self.__swReq;
      if (!req || !req.c) { fwd('webpack require not exposed', ''); return; }
      for (const id of Object.keys(req.c)) {
        try {
          const d = req.c[id] && req.c[id].exports && req.c[id].exports.default;
          if (d && typeof d.getInstance === 'function' && String(d.getInstance).includes('createQueryBuilder')) { d.getInstance(); fwd('database client ready', id); return; }
        } catch (e) { /* a throwing export getter must not abort the search */ }
      }
      fwd('database client accessor not found', '');
    } catch (e) { fwd('database client setup failed', errText(e)); }
  };
  if (typeof self.importScripts === 'function') {
    const imp = self.importScripts;
    self.importScripts = function (...urls) { const r = imp.apply(this, urls); if (urls.some((u) => /sync-v2-sub-worker/.test(String(u)))) ensureDatabaseClient(); return r; };
  }
  if (typeof IDBDatabase !== 'undefined') {
    const tx = IDBDatabase.prototype.transaction;
    IDBDatabase.prototype.transaction = function (...a) {
      const t = tx.apply(this, a);
      t.addEventListener('abort', () => fwd('idb-abort', errText(t.error)));
      return t;
    };
    for (const m of ['add', 'put']) {
      const o = IDBObjectStore.prototype[m];
      IDBObjectStore.prototype[m] = function (...a) {
        try { const r = o.apply(this, a); r.addEventListener('error', () => fwd('idb-' + m + '-error', this.name + ' ' + errText(r.error))); return r; }
        catch (e) { fwd('idb-' + m + '-throw', this.name + ' ' + errText(e)); throw e; }
      };
    }
    const open = IDBFactory.prototype.open;
    IDBFactory.prototype.open = function (...a) {
      const r = open.apply(this, a);
      r.addEventListener('error', () => fwd('idb-open-error', errText(r.error)));
      r.addEventListener('blocked', () => fwd('idb-open-blocked', ''));
      return r;
    };
  }
})();

