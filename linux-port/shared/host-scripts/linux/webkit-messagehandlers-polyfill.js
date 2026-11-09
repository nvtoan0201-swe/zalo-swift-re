// Olaz Linux host script (NEW, not from the macOS app). NOT USED by the chosen WebKit webhost:
// kept only for a Chromium-based host (QtWebEngine/CEF) that lacks WebKit's messageHandlers. If ever used, inject it at
// document start right after the channel library (qwebchannel.js) and before every other host script.
//
// The Olaz host scripts talk to native code through WebKit's `window.webkit.messageHandlers.<name>
// .postMessage(body)` (a Promise for handlers with a reply). QtWebEngine has no such API, so this
// recreates it over QWebChannel, keeping message order per page (messages posted before the channel
// is up are queued in order). Native replies with `window.__olazReply(id, value, error)`.
(() => {
  'use strict';
  if (window.webkit && window.webkit.messageHandlers) return;
  const queue = [];
  const pending = new Map();
  let channel = null;
  let seq = 0;
  const send = (name, body, wantReply) => {
    const id = wantReply ? ++seq : 0;
    const msg = JSON.stringify({ id, name, body: body === undefined ? null : body });
    if (channel) channel.objects.olaz.post(msg); else queue.push(msg);
    if (!wantReply) return undefined;
    return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
  };
  const handler = (name, wantReply) => Object.freeze({ postMessage: (body) => send(name, body, wantReply) });
  window.webkit = Object.freeze({
    messageHandlers: Object.freeze({
      zlog: handler('zlog', false),
      zstate: handler('zstate', false),
      znet: handler('znet', true),
      zbridge: handler('zbridge', true),
    }),
  });
  window.__olazReply = (id, value, error) => {
    const p = pending.get(id);
    if (!p) return;
    pending.delete(id);
    if (error !== undefined && error !== null) p.reject(error); else p.resolve(value);
  };
  if (typeof QWebChannel !== 'function' || !window.qt || !window.qt.webChannelTransport) {
    // Not running in QtWebEngine (e.g. a unit test page): leave messages queued.
    return;
  }
  new QWebChannel(window.qt.webChannelTransport, (c) => {
    channel = c;
    for (const m of queue.splice(0)) c.objects.olaz.post(m);
  });
})();
