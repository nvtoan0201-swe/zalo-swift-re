// EXTRACTED VERBATIM from Sources/Olaz/ZaloRuntime/OfficialClientHost.swift (AsarSchemeHandler.workerDebugEval, DEBUG builds only) — do not edit here; re-run tools/sync-from-macos.sh
  // Debug builds: a probe worker created from the dev eval channel can run code in this scope.
  self.addEventListener('message', async (e) => {
    const d = e.data;
    if (!d || typeof d !== 'object' || typeof d.__zhostEval !== 'string') return;
    e.stopImmediatePropagation();
    try { const r = await (0, eval)(d.__zhostEval); self.postMessage({ __zhostEvalResult: JSON.stringify(r === undefined ? null : r) }); }
    catch (err) { self.postMessage({ __zhostEvalResult: JSON.stringify({ error: errText(err) }) }); }
  });
