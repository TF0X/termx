// UI for the glass terminal: tab strip, window chrome, and one xterm.js
// instance per tab. Every shell lives in Rust behind a ConPTY; this side only
// renders bytes and forwards keystrokes.
(function () {
  "use strict";

  // Same idea as anion's glass levels: the window carries the acrylic blur,
  // and this is the only tint layered on top of it.
  var TINT = "16,18,23";
  var LEVELS = [
    { alpha: 0.94, glyph: "○", name: "Solid" },
    { alpha: 0.5,  glyph: "◑", name: "Frosted" },
    { alpha: 0.14, glyph: "●", name: "Clear" }
  ];

  var THEME = {
    background: "rgba(0,0,0,0)",
    foreground: "#e4e6ec",
    cursor: "#a8d4ff",
    cursorAccent: "#101217",
    selectionBackground: "rgba(168,212,255,0.28)",
    black: "#2b2e36",   brightBlack: "#6b7080",
    red: "#ff6b7a",     brightRed: "#ff929d",
    green: "#7ee2a8",   brightGreen: "#a3f0c4",
    yellow: "#f5d37a",  brightYellow: "#ffe4a1",
    blue: "#7ab4ff",    brightBlue: "#a4cbff",
    magenta: "#d49cff", brightMagenta: "#e4bdff",
    cyan: "#6fe0e6",    brightCyan: "#9df0f3",
    white: "#d6d9e0",   brightWhite: "#ffffff"
  };

  var tabs = {};      // id -> { term, fit, pane, tab, name }
  var order = [];
  var active = null;
  var nextId = 1;
  var level = 1;
  var fontSize = 14;

  var stage = document.getElementById("stage");
  var strip = document.getElementById("tabs");
  var glassBtn = document.getElementById("glass");

  function send(action, extra) {
    var payload = { action: action };
    if (extra) for (var k in extra) payload[k] = extra[k];
    try {
      window.ipc.postMessage(JSON.stringify(payload));
    } catch (e) { /* host not ready */ }
  }

  function el(tag, cls) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    return n;
  }

  // ---- glass ---------------------------------------------------------------

  function setLevel(next, persist) {
    level = ((next % LEVELS.length) + LEVELS.length) % LEVELS.length;
    document.body.style.setProperty("--tint", TINT);
    document.body.style.setProperty("--alpha", LEVELS[level].alpha);
    glassBtn.textContent = LEVELS[level].glyph;
    glassBtn.title = "Glass: " + LEVELS[level].name;
    glassBtn.classList.toggle("lit", level > 0);
    if (persist) send("glass", { level: level });
  }

  // ---- tabs ----------------------------------------------------------------

  function fitActive() {
    var t = tabs[active];
    if (!t) return;
    try { t.fit.fit(); } catch (e) { /* pane not laid out yet */ }
  }

  function activate(id) {
    if (!tabs[id]) return;
    active = id;
    for (var k in tabs) {
      var on = +k === id;
      tabs[k].pane.classList.toggle("on", on);
      tabs[k].tab.classList.toggle("on", on);
    }
    // Hidden panes cannot measure, so a tab is fitted when it becomes visible.
    requestAnimationFrame(function () {
      fitActive();
      tabs[id] && tabs[id].term.focus();
    });
  }

  function keys(id) {
    return function (e) {
      var ctrl = e.ctrlKey && !e.altKey && !e.metaKey;
      var k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
      var hit = null;

      if (ctrl && e.shiftKey && k === "c") hit = copy;
      else if (ctrl && e.shiftKey && k === "v") hit = paste;
      else if (ctrl && !e.shiftKey && k === "v") hit = paste;
      // Ctrl+C copies when something is selected, otherwise it is SIGINT.
      else if (ctrl && !e.shiftKey && k === "c" && tabs[id].term.hasSelection()) hit = copy;
      else if (ctrl && e.shiftKey && k === "t") hit = newTab;
      else if (ctrl && e.shiftKey && k === "w") hit = function () { closeTab(active, true); };
      else if (ctrl && k === "Tab") hit = function () { cycle(e.shiftKey ? -1 : 1); };
      else if (ctrl && (k === "=" || k === "+")) hit = function () { zoom(fontSize + 1); };
      else if (ctrl && k === "-") hit = function () { zoom(fontSize - 1); };
      else if (ctrl && k === "0") hit = function () { zoom(14); };

      if (!hit) return true;
      // Swallow keyup/keypress too, but only act once. preventDefault stops
      // the browser's own paste from firing a second copy of the text.
      e.preventDefault();
      if (e.type === "keydown") hit();
      return false;
    };
  }

  function copy() {
    var t = tabs[active];
    if (!t || !t.term.hasSelection()) return;
    send("copy", { text: t.term.getSelection() });
    t.term.clearSelection();
  }

  function paste() {
    if (active != null) send("paste", { id: active });
  }

  function cycle(dir) {
    if (order.length < 2) return;
    var i = order.indexOf(active);
    activate(order[(i + dir + order.length) % order.length]);
  }

  function zoom(size) {
    fontSize = Math.max(9, Math.min(32, size));
    for (var k in tabs) tabs[k].term.options.fontSize = fontSize;
    fitActive();
    send("font", { size: fontSize });
  }

  function newTab() {
    var id = nextId++;

    var pane = el("div", "pane");
    stage.appendChild(pane);

    var tab = el("div", "tab");
    var name = el("span", "name");
    name.textContent = "shell";
    var x = el("button", "x");
    x.textContent = "×";
    x.title = "Close tab";
    tab.appendChild(name);
    tab.appendChild(x);
    strip.appendChild(tab);

    tab.addEventListener("mousedown", function (e) {
      if (e.button === 1) { e.preventDefault(); closeTab(id, true); }
      else if (e.button === 0 && e.target !== x) activate(id);
    });
    x.addEventListener("click", function () { closeTab(id, true); });

    var term = new Terminal({
      allowTransparency: true,
      fontFamily: '"Cascadia Mono", "Cascadia Code", Consolas, monospace',
      fontSize: fontSize,
      lineHeight: 1.12,
      cursorBlink: true,
      cursorStyle: "bar",
      scrollback: 10000,
      theme: THEME
    });
    var fit = new FitAddon.FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon.WebLinksAddon(function (ev, uri) {
      if (ev.ctrlKey) send("open", { url: uri });
    }));
    term.open(pane);

    term.onData(function (d) { send("input", { id: id, data: d }); });
    term.onBinary(function (d) { send("input", { id: id, bin: btoa(d) }); });
    term.onResize(function (s) { send("pty_resize", { id: id, cols: s.cols, rows: s.rows }); });
    term.onTitleChange(function (t) {
      // PowerShell sets the title to the full exe path; the leaf reads better.
      var leaf = (t || "").split(/[\\/]/).pop();
      name.textContent = leaf || "shell";
      tab.title = t || "";
    });
    term.attachCustomKeyEventHandler(keys(id));

    // Right-click: copy when there is a selection, paste when there is not.
    pane.addEventListener("contextmenu", function (e) {
      e.preventDefault();
      if (term.hasSelection()) copy(); else paste();
    });

    tabs[id] = { term: term, fit: fit, pane: pane, tab: tab, name: name };
    order.push(id);

    // Size against the real pane before the shell starts, so its first
    // prompt is laid out for the right width.
    pane.classList.add("on");
    try { fit.fit(); } catch (e) { /* zero-size during startup */ }
    send("spawn", { id: id, cols: term.cols, rows: term.rows });
    activate(id);
  }

  function closeTab(id, kill) {
    var t = tabs[id];
    if (!t) return;
    if (kill) send("kill", { id: id });
    t.term.dispose();
    t.pane.remove();
    t.tab.remove();
    delete tabs[id];

    var i = order.indexOf(id);
    order.splice(i, 1);
    if (!order.length) { send("close"); return; }
    if (active === id) activate(order[Math.min(i, order.length - 1)]);
  }

  // ---- calls from Rust -----------------------------------------------------

  window.__init = function (s) {
    if (typeof s.font === "number") fontSize = s.font;
    setLevel(typeof s.glass === "number" ? s.glass : 1, false);
    if (!order.length) newTab();
  };

  window.__out = function (id, b64) {
    var t = tabs[id];
    if (!t) return;
    var raw = atob(b64);
    var bytes = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    // Bytes, not a string: a UTF-8 sequence split across two reads is
    // reassembled by xterm's decoder instead of turning into U+FFFD.
    t.term.write(bytes);
  };

  window.__exit = function (id) { closeTab(id, false); };

  window.__fail = function (id, msg) {
    var t = tabs[id];
    if (t) t.term.write("\r\n\x1b[31m" + msg + "\x1b[0m\r\n");
  };

  window.__paste = function (id, text) {
    var t = tabs[id];
    if (t && text) t.term.paste(text);
  };

  window.__focus = function () {
    var t = tabs[active];
    if (t) t.term.focus();
  };

  // ---- window chrome -------------------------------------------------------

  document.querySelector(".wbtn.close").addEventListener("click", function () { send("close"); });
  document.querySelector(".wbtn.min").addEventListener("click", function () { send("minimize"); });
  document.querySelector(".wbtn.max").addEventListener("click", function () { send("maximize"); });
  document.getElementById("add").addEventListener("click", newTab);
  glassBtn.addEventListener("click", function () { setLevel(level + 1, true); });

  // Deliberately no dblclick-to-maximize: drag_window() takes over the mouse
  // on mousedown, so two quick drags would surface as a dblclick.
  document.getElementById("drag").addEventListener("mousedown", function (e) {
    if (e.button !== 0) return;
    e.preventDefault();
    send("drag");
  });

  ["n", "s", "w", "e", "nw", "ne", "sw", "se"].forEach(function (dir) {
    var g = el("div", "rz " + dir);
    g.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      send("resize", { dir: dir });
    });
    document.body.appendChild(g);
  });

  document.addEventListener("contextmenu", function (e) { e.preventDefault(); });

  var resizeTimer = null;
  addEventListener("resize", function () {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(fitActive, 30);
  }, { passive: true });

  setLevel(level, false);
  send("ready");
})();
