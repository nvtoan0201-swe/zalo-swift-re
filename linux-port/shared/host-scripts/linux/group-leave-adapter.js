// Olaz (Linux) leave-group adapter (index.html only): "Rời nhóm" from the conversation menu runs what the official
// conversation menu's confirm runs, `fetcher.leaveGroup(groupId)` (the fetcher singleton next to `removeFriend` and
// `disperseGroup`; it reports the leave, calls the API and drops the group from storage). A group the user created
// passes to another member on the server side, as in the official menu.
//
// Rust → JS: window.__zaloGroupLeave.isOwner(id) → bool, .leave(id) → Promise<{ok, text}>.
(() => {
  'use strict';
  if (!/index\.html$/.test(location.pathname)) return;

  const TEXT = {
    left: 'Bạn đã rời nhóm',
    noNetwork: 'Vui lòng kiểm tra lại kết nối mạng của bạn.',   // STR_CHECK_NET
    failed: 'Rời nhóm không thành công. Vui lòng thử lại.',
    unavailable: 'Chưa thể rời nhóm lúc này. Hãy thử lại sau ít phút.',
  };
  const safe = (f, d) => { try { return f(); } catch { return d; } };
  const S = {};
  const resolve = () => {
    const req = window.__zaloRequire;
    if (S.fetcher && S.groups) return true;
    if (!req || typeof window.__zaloEachExport !== 'function') return false;
    window.__zaloEachExport(req, (ex) => {
      if (!ex || (typeof ex !== 'object' && typeof ex !== 'function')) return;
      try {
        // The host answers unknown preload globals with catch-all placeholders; real modules don't.
        if (ex.__olaz_not_a_member__ !== undefined) return;
        // The fetcher is an instance; the API class under it has static methods of the same names (no report, no storage update).
        if (!S.fetcher && typeof ex === 'object' && typeof ex.leaveGroup === 'function' && typeof ex.disperseGroup === 'function' && typeof ex.removeFriend === 'function') S.fetcher = ex;
        if (!S.groups && typeof ex.getGroupByIdSync === 'function' && typeof ex.getGroupsListSync === 'function') S.groups = ex;
      } catch {}
    });
    return !!S.fetcher;
  };
  const myId = () => String(safe(() => window.__zaloStore.getState().user.userId, '') || '');

  window.__zaloGroupLeave = {
    isOwner(id) {
      resolve();
      const g = safe(() => S.groups.getGroupByIdSync(String(id)), null);
      const me = myId();
      return !!(g && me && String(g.creatorId || '') === me);
    },
    async leave(id) {
      if (!/^g/.test(String(id || ''))) return { ok: false, text: TEXT.failed };
      if (!resolve()) return { ok: false, text: TEXT.unavailable };
      try {
        await S.fetcher.leaveGroup(String(id));
        return { ok: true, text: TEXT.left };
      } catch (e) {
        return { ok: false, text: e && e.code === 'ERR_NO_NETWORK' ? TEXT.noNetwork : TEXT.failed };
      }
    },
  };
})();
