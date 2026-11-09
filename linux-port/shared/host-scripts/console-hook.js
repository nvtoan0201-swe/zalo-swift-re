// EXTRACTED VERBATIM from Sources/Olaz/ZaloRuntime/OfficialClientHost.swift (ConsoleHandler.script) — do not edit here; re-run tools/sync-from-macos.sh
(() => {
  const post = (lvl, a) => { try { webkit.messageHandlers.zlog.postMessage(lvl + ' ' + a.map(x => { try { return typeof x === 'string' ? x : (x instanceof Error ? x.message + ' | ' + (x.stack || '').split('\n').slice(0, 3).join(' < ') : JSON.stringify(x)); } catch { return String(x); } }).join(' ').slice(0, 1200)); } catch {} };
  for (const lvl of ['log','warn','error','info']) { const o = console[lvl]; console[lvl] = (...a) => { post('[' + lvl + ']', a); return o.apply(console, a); }; }
  addEventListener('error', e => post('[uncaught]', [e.message + ' @ ' + e.filename + ':' + e.lineno + ':' + e.colno]));
  addEventListener('unhandledrejection', e => post('[unhandled]', [e.reason instanceof Error ? e.reason : (() => { try { const j = JSON.stringify(e.reason, (k, v) => typeof v === 'string' && v.length > 60 ? v.slice(0, 60) + '…' : v); return j && j !== '{}' ? j.slice(0, 400) : String(e.reason); } catch { return String(e.reason); } })()]));
})();
