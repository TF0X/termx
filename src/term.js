// UI for the glass terminal: tabs of split panes, the profile dropdown, window
// chrome and the wobbly drag. Every shell lives in Rust behind a ConPTY; this
// side only renders bytes and forwards keystrokes.
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

  var BADGES = {
    pwsh:       { label: "PS", color: "#2b7de9" },
    powershell: { label: "PS", color: "#1d4f9c" },
    cmd:        { label: "C:", color: "#5c6370" },
    gitbash:    { label: "$",  color: "#f05033" },
    wsl:        { label: "λ",  color: "#e95420" },
    custom:     { label: "★",  color: "#6d7cff" }
  };

  var profiles = [{ name: "Shell", kind: "powershell" }];

  // A tab's layout is a binary tree: { pane: id } leaves, and
  // { dir: "row" | "col", a, b, ratio } splits.
  var panes = {};   // id -> { term, fit, cell, host, tab, profile, title, opened }
  var tabs = {};    // id -> { root, focus, stage, chip, name, badge }
  var order = [];
  var activeTab = null;
  var nextPane = 1;
  var nextTab = 1;

  var level = 1;
  var fontSize = 14;
  var wobbleOn = true;
  var themeOn = true;

  var app = document.getElementById("app");
  var stage = document.getElementById("stage");
  var strip = document.getElementById("tabs");
  var glassBtn = document.getElementById("glass");
  var moreBtn = document.getElementById("more");
  var menu = document.getElementById("menu");

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

  function badge(kind) {
    var b = BADGES[kind] || BADGES.custom;
    var n = el("span", "badge");
    n.textContent = b.label;
    n.style.setProperty("--c", b.color);
    return n;
  }

  // ---- glass ---------------------------------------------------------------

  function setLevel(next, persist) {
    level = ((next % LEVELS.length) + LEVELS.length) % LEVELS.length;
    app.style.setProperty("--tint", TINT);
    app.style.setProperty("--alpha", LEVELS[level].alpha);
    glassBtn.textContent = LEVELS[level].glyph;
    glassBtn.title = "Glass: " + LEVELS[level].name;
    glassBtn.classList.toggle("lit", level > 0);
    if (persist) send("glass", { level: level });
  }

  // ---- layout tree ---------------------------------------------------------

  function leaves(node, out) {
    out = out || [];
    if (node.pane != null) out.push(node.pane);
    else { leaves(node.a, out); leaves(node.b, out); }
    return out;
  }

  function replaceLeaf(node, id, withNode) {
    if (node.pane != null) return node.pane === id ? withNode : node;
    node.a = replaceLeaf(node.a, id, withNode);
    node.b = replaceLeaf(node.b, id, withNode);
    return node;
  }

  // Drops a leaf; its sibling takes the parent split's place.
  function removeLeaf(node, id) {
    if (node.pane != null) return node.pane === id ? null : node;
    var a = removeLeaf(node.a, id);
    var b = removeLeaf(node.b, id);
    if (!a) return b;
    if (!b) return a;
    node.a = a;
    node.b = b;
    return node;
  }

  function build(node) {
    if (node.pane != null) return panes[node.pane].cell;
    var box = el("div", "split " + node.dir);
    var a = el("div", "slot");
    var b = el("div", "slot");
    var dv = el("div", "divider");
    a.style.flex = node.ratio + " 1 0";
    b.style.flex = (1 - node.ratio) + " 1 0";
    a.appendChild(build(node.a));
    b.appendChild(build(node.b));
    dividerDrag(dv, box, node, a, b);
    box.appendChild(a);
    box.appendChild(dv);
    box.appendChild(b);
    return box;
  }

  function render(t) {
    // Re-parenting keeps each xterm (and its WebGL canvas) alive; only the
    // split scaffolding around them is rebuilt.
    t.stage.textContent = "";
    t.stage.appendChild(build(t.root));
    t.stage.classList.toggle("multi", t.root.pane == null);
    markFocus(t);
  }

  function dividerDrag(dv, box, node, a, b) {
    dv.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      var r = box.getBoundingClientRect();
      dv.classList.add("drag");
      function move(ev) {
        var f = node.dir === "row"
          ? (ev.clientX - r.left) / r.width
          : (ev.clientY - r.top) / r.height;
        node.ratio = Math.max(0.08, Math.min(0.92, f));
        a.style.flex = node.ratio + " 1 0";
        b.style.flex = (1 - node.ratio) + " 1 0";
        scheduleFit();
      }
      function up() {
        dv.classList.remove("drag");
        removeEventListener("mousemove", move);
        removeEventListener("mouseup", up);
        scheduleFit();
      }
      addEventListener("mousemove", move);
      addEventListener("mouseup", up);
    });
  }

  var fitQueued = false;
  function scheduleFit() {
    if (fitQueued) return;
    fitQueued = true;
    requestAnimationFrame(function () {
      fitQueued = false;
      fitTab(tabs[activeTab]);
    });
  }

  function fitTab(t) {
    if (!t) return;
    leaves(t.root).forEach(function (id) {
      try { panes[id].fit.fit(); } catch (e) { /* not laid out yet */ }
    });
  }

  // ---- panes ---------------------------------------------------------------

  function newPane(tid, profile) {
    var id = nextPane++;
    var cell = el("div", "cell");
    var host = el("div", "pane");
    cell.appendChild(host);
    panes[id] = {
      term: null, fit: null, cell: cell, host: host,
      tab: tid, profile: profile, title: profiles[profile] ? profiles[profile].name : "shell"
    };
    cell.addEventListener("mousedown", function () { focusPane(id); }, true);
    cell.addEventListener("contextmenu", function (e) {
      e.preventDefault();
      var p = panes[id];
      if (p && p.term.hasSelection()) copy(); else paste();
    });
    return id;
  }

  // Opening needs the host attached and sized, so it runs after render().
  function startPane(id) {
    var p = panes[id];
    var term = new Terminal({
      allowTransparency: true,
      fontFamily: '"Cascadia Mono", "Cascadia Code", Consolas, "Segoe UI Symbol", monospace',
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
    term.open(p.host);

    // GPU rendering: far less CPU per frame than the DOM renderer. If the GPU
    // drops the context (driver reset, sleep), fall back to DOM silently.
    try {
      var gl = new WebglAddon.WebglAddon();
      gl.onContextLoss(function () { gl.dispose(); });
      term.loadAddon(gl);
    } catch (e) { /* no WebGL2: DOM renderer it is */ }

    p.term = term;
    p.fit = fit;

    term.onData(function (d) { send("input", { id: id, data: d }); });
    term.onBinary(function (d) { send("input", { id: id, bin: btoa(d) }); });
    term.onResize(function (s) { send("pty_resize", { id: id, cols: s.cols, rows: s.rows }); });
    term.onTitleChange(function (t) {
      // PowerShell's default title is the full exe path; the leaf reads better.
      p.title = (t || "").split(/[\\/]/).pop() || "shell";
      if (tabs[p.tab] && tabs[p.tab].focus === id) syncChip(tabs[p.tab]);
    });
    term.attachCustomKeyEventHandler(keys);
    term.textarea.addEventListener("focus", function () { focusPane(id, true); });

    try { fit.fit(); } catch (e) { /* zero-size during startup */ }
    send("spawn", { id: id, cols: term.cols, rows: term.rows, profile: p.profile });
  }

  function focusPane(id, fromTerm) {
    var p = panes[id];
    if (!p) return;
    var t = tabs[p.tab];
    t.focus = id;
    markFocus(t);
    syncChip(t);
    if (!fromTerm && p.term) p.term.focus();
  }

  function markFocus(t) {
    leaves(t.root).forEach(function (id) {
      panes[id].cell.classList.toggle("focus", id === t.focus);
    });
  }

  function syncChip(t) {
    var p = panes[t.focus];
    if (!p) return;
    t.name.textContent = p.title;
    var prof = profiles[p.profile] || profiles[0];
    var fresh = badge(prof.kind);
    t.chip.replaceChild(fresh, t.badge);
    t.badge = fresh;
  }

  function split(side, profile) {
    var t = tabs[activeTab];
    if (!t) return;
    var cur = t.focus;
    var id = newPane(activeTab, profile != null ? profile : panes[cur].profile);
    var oldLeaf = { pane: cur };
    var newLeaf = { pane: id };
    var first = side === "left" || side === "up";
    t.root = replaceLeaf(t.root, cur, {
      dir: side === "left" || side === "right" ? "row" : "col",
      a: first ? newLeaf : oldLeaf,
      b: first ? oldLeaf : newLeaf,
      ratio: 0.5
    });
    t.focus = id;
    render(t);
    fitTab(t);
    startPane(id);
    focusPane(id);
  }

  function closePane(id, kill) {
    var p = panes[id];
    if (!p) return;
    var t = tabs[p.tab];
    if (kill) send("kill", { id: id });
    if (p.term) p.term.dispose();
    delete panes[id];

    t.root = removeLeaf(t.root, id);
    if (!t.root) { dropTab(p.tab); return; }
    if (t.focus === id) t.focus = leaves(t.root)[0];
    render(t);
    requestAnimationFrame(function () {
      fitTab(t);
      if (activeTab === p.tab) focusPane(t.focus);
    });
  }

  // Alt+Arrow: the nearest pane lying wholly on that side of the current one.
  function moveFocus(dir) {
    var t = tabs[activeTab];
    if (!t) return;
    var cur = panes[t.focus].cell.getBoundingClientRect();
    var cx = cur.left + cur.width / 2;
    var cy = cur.top + cur.height / 2;
    var best = null;
    var bestD = Infinity;
    leaves(t.root).forEach(function (id) {
      if (id === t.focus) return;
      var r = panes[id].cell.getBoundingClientRect();
      var ok = dir === "left" ? r.right <= cur.left + 2
        : dir === "right" ? r.left >= cur.right - 2
        : dir === "up" ? r.bottom <= cur.top + 2
        : r.top >= cur.bottom - 2;
      if (!ok) return;
      var d = Math.hypot(r.left + r.width / 2 - cx, r.top + r.height / 2 - cy);
      if (d < bestD) { bestD = d; best = id; }
    });
    if (best != null) focusPane(best);
  }

  // ---- tabs ----------------------------------------------------------------

  function newTab(profile) {
    profile = profile || 0;
    var tid = nextTab++;
    var tstage = el("div", "tabstage");
    stage.appendChild(tstage);

    var chip = el("div", "tab");
    var b = badge((profiles[profile] || profiles[0]).kind);
    var name = el("span", "name");
    var x = el("button", "x");
    x.textContent = "×";
    x.title = "Close tab";
    chip.appendChild(b);
    chip.appendChild(name);
    chip.appendChild(x);
    strip.appendChild(chip);

    chip.addEventListener("mousedown", function (e) {
      if (e.button === 1) { e.preventDefault(); closeTab(tid); }
      else if (e.button === 0 && e.target !== x) activate(tid);
    });
    x.addEventListener("click", function () { closeTab(tid); });

    var id = newPane(tid, profile);
    var t = { root: { pane: id }, focus: id, stage: tstage, chip: chip, name: name, badge: b };
    tabs[tid] = t;
    order.push(tid);

    activate(tid, true);
    render(t);
    startPane(id);
    syncChip(t);
    focusPane(id);
  }

  function activate(tid, quiet) {
    if (!tabs[tid]) return;
    activeTab = tid;
    for (var k in tabs) {
      var on = +k === tid;
      tabs[k].stage.classList.toggle("on", on);
      tabs[k].chip.classList.toggle("on", on);
    }
    if (quiet) return;
    // Hidden tabs cannot measure, so a tab is fitted when it becomes visible.
    requestAnimationFrame(function () {
      var t = tabs[tid];
      if (!t) return;
      fitTab(t);
      focusPane(t.focus);
    });
  }

  function closeTab(tid) {
    var t = tabs[tid];
    if (!t) return;
    leaves(t.root).forEach(function (id) {
      send("kill", { id: id });
      if (panes[id].term) panes[id].term.dispose();
      delete panes[id];
    });
    dropTab(tid);
  }

  function dropTab(tid) {
    var t = tabs[tid];
    t.stage.remove();
    t.chip.remove();
    delete tabs[tid];
    var i = order.indexOf(tid);
    order.splice(i, 1);
    if (!order.length) { send("close"); return; }
    if (activeTab === tid) activate(order[Math.min(i, order.length - 1)]);
  }

  function cycle(dir) {
    if (order.length < 2) return;
    var i = order.indexOf(activeTab);
    activate(order[(i + dir + order.length) % order.length]);
  }

  // ---- clipboard, zoom, keys -----------------------------------------------

  function focused() {
    var t = tabs[activeTab];
    return t ? panes[t.focus] : null;
  }

  function copy() {
    var p = focused();
    if (!p || !p.term.hasSelection()) return;
    send("copy", { text: p.term.getSelection() });
    p.term.clearSelection();
  }

  function paste() {
    var t = tabs[activeTab];
    if (t) send("paste", { id: t.focus });
  }

  function zoom(size) {
    fontSize = Math.max(9, Math.min(32, size));
    for (var k in panes) if (panes[k].term) panes[k].term.options.fontSize = fontSize;
    scheduleFit();
    send("font", { size: fontSize });
  }

  var ARROWS = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };

  function keys(e) {
    var ctrl = e.ctrlKey && !e.altKey && !e.metaKey;
    var altShift = e.altKey && e.shiftKey && !e.ctrlKey;
    var altOnly = e.altKey && !e.shiftKey && !e.ctrlKey;
    var k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    var digit = /^Digit([1-9])$/.exec(e.code);
    var hit = null;

    if (ctrl && e.shiftKey && k === "c") hit = copy;
    else if (ctrl && k === "v") hit = paste;
    // Ctrl+C copies when something is selected, otherwise it is SIGINT.
    else if (ctrl && !e.shiftKey && k === "c" && focused() && focused().term.hasSelection()) hit = copy;
    else if (ctrl && e.shiftKey && k === "t") hit = function () { newTab(0); };
    else if (ctrl && e.shiftKey && digit) hit = function () {
      var n = +digit[1] - 1;
      if (profiles[n]) newTab(n);
    };
    else if (ctrl && e.shiftKey && k === "w") hit = function () { var t = tabs[activeTab]; if (t) closePane(t.focus, true); };
    else if (ctrl && e.shiftKey && k === "e") hit = function () { split("right"); };
    else if (ctrl && e.shiftKey && k === "o") hit = function () { split("down"); };
    else if (ctrl && e.shiftKey && k === " ") hit = function () { toggleMenu(); };
    else if (ctrl && k === "Tab") hit = function () { cycle(e.shiftKey ? -1 : 1); };
    else if (altShift && ARROWS[e.key]) hit = function () { split(ARROWS[e.key]); };
    else if (altOnly && ARROWS[e.key]) hit = function () { moveFocus(ARROWS[e.key]); };
    else if (ctrl && !e.shiftKey && (k === "=" || k === "+")) hit = function () { zoom(fontSize + 1); };
    else if (ctrl && !e.shiftKey && k === "-") hit = function () { zoom(fontSize - 1); };
    else if (ctrl && !e.shiftKey && k === "0") hit = function () { zoom(14); };

    if (!hit) return true;
    // Swallow keyup/keypress too, but only act once. preventDefault stops
    // the browser's own paste from firing a second copy of the text.
    e.preventDefault();
    if (e.type === "keydown") hit();
    return false;
  }

  // ---- dropdown ------------------------------------------------------------

  function item(label, key, onClick, lead) {
    var n = el("div", "mi");
    if (lead) n.appendChild(lead);
    var l = el("span", "label");
    l.textContent = label;
    n.appendChild(l);
    if (key) {
      var kk = el("span", "key");
      kk.textContent = key;
      n.appendChild(kk);
    }
    n.addEventListener("mousedown", function (e) { e.preventDefault(); });
    n.addEventListener("click", function (e) {
      closeMenu();
      onClick(e);
    });
    return n;
  }

  function icon(ch, cls) {
    var n = el("span", cls || "ico");
    n.textContent = ch;
    return n;
  }

  function fillMenu() {
    menu.textContent = "";
    profiles.forEach(function (p, i) {
      menu.appendChild(item(p.name, i < 9 ? "Ctrl+Shift+" + (i + 1) : "", function (e) {
        if (e.altKey) split("right", i); else newTab(i);
      }, badge(p.kind)));
    });
    var hint = el("div", "mhint");
    hint.textContent = "Alt+click a profile to open it in a split";
    menu.appendChild(hint);
    menu.appendChild(el("div", "msep"));
    menu.appendChild(item("Split right", "Ctrl+Shift+E", function () { split("right"); }, icon("◧")));
    menu.appendChild(item("Split down", "Ctrl+Shift+O", function () { split("down"); }, icon("⬒")));
    menu.appendChild(item("Split left", "Alt+Shift+←", function () { split("left"); }, icon("◨")));
    menu.appendChild(item("Split up", "Alt+Shift+↑", function () { split("up"); }, icon("⬓")));
    menu.appendChild(el("div", "msep"));
    menu.appendChild(item("Shell theme", "new panes", function () {
      themeOn = !themeOn;
      send("theme", { on: themeOn });
    }, icon(themeOn ? "✓" : "", "check")));
    menu.appendChild(item("Wobbly drag", "", function () {
      wobbleOn = !wobbleOn;
      send("wobble", { on: wobbleOn });
    }, icon(wobbleOn ? "✓" : "", "check")));
  }

  function toggleMenu() {
    if (menu.classList.contains("open")) { closeMenu(); return; }
    fillMenu();
    var r = moreBtn.getBoundingClientRect();
    menu.style.left = Math.max(6, Math.min(r.left - 30, innerWidth - 262)) + "px";
    menu.classList.add("open");
    moreBtn.classList.add("open");
  }

  function closeMenu() {
    if (!menu.classList.contains("open")) return;
    menu.classList.remove("open");
    moreBtn.classList.remove("open");
    var p = focused();
    if (p && p.term) p.term.focus();
  }

  addEventListener("mousedown", function (e) {
    if (!menu.contains(e.target) && e.target !== moreBtn) closeMenu();
  }, true);
  addEventListener("keydown", function (e) {
    if (e.key === "Escape") closeMenu();
  }, true);
  addEventListener("blur", closeMenu);

  // ---- wobbly drag ---------------------------------------------------------
  // Windows moves the window; we warp the page inside it. Each corner of #app
  // hangs on a spring: the corner you grabbed follows the cursor, the far ones
  // lag behind and overshoot, and a projective transform (matrix3d) maps the
  // page onto that quad. The page shrinks a little while lifted so the warp
  // has transparent room to move in; Rust turns the blur off for the duration.

  var SCALE = 0.94;   // page size while lifted
  var K = 240;        // spring stiffness
  var DAMP = 12;      // spring damping: lower = jigglier
  var LAG = 0.85;     // how much of each window step the far corners resist

  var wob = {
    dragging: false,
    running: false,
    s: 1,
    grab: [0.5, 0.5],
    last: null,
    prev: 0,
    c: [[0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]] // dx, dy, vx, vy
  };
  var CORNERS = [[0, 0], [1, 0], [1, 1], [0, 1]];

  function dragStart(e) {
    if (!wobbleOn) { send("drag", { wobble: false }); return; }
    wob.grab = [e.clientX / innerWidth, e.clientY / innerHeight];
    wob.last = null;
    wob.dragging = true;
    app.classList.add("lifted");
    send("drag", { wobble: true });
    if (!wob.running) {
      wob.running = true;
      wob.prev = performance.now();
      requestAnimationFrame(tick);
    }
  }

  window.__moved = function (x, y) {
    var dpr = devicePixelRatio || 1;
    x /= dpr;
    y /= dpr;
    if (wob.last) {
      var dx = x - wob.last[0];
      var dy = y - wob.last[1];
      for (var i = 0; i < 4; i++) {
        var w = Math.hypot(CORNERS[i][0] - wob.grab[0], CORNERS[i][1] - wob.grab[1]) / Math.SQRT2;
        wob.c[i][0] -= dx * w * LAG;
        wob.c[i][1] -= dy * w * LAG;
      }
    }
    wob.last = [x, y];
  };

  window.__dragEnd = function () { wob.dragging = false; };

  function homography(W, H, q) {
    var x0 = q[0][0], y0 = q[0][1], x1 = q[1][0], y1 = q[1][1];
    var x2 = q[2][0], y2 = q[2][1], x3 = q[3][0], y3 = q[3][1];
    var dx1 = x1 - x2, dx2 = x3 - x2, dx3 = x0 - x1 + x2 - x3;
    var dy1 = y1 - y2, dy2 = y3 - y2, dy3 = y0 - y1 + y2 - y3;
    var det = dx1 * dy2 - dx2 * dy1 || 1e-9;
    var g = (dx3 * dy2 - dx2 * dy3) / det;
    var h = (dx1 * dy3 - dx3 * dy1) / det;
    var a = (x1 - x0 + g * x1) / W, b = (x3 - x0 + h * x3) / H, c = x0;
    var d = (y1 - y0 + g * y1) / W, e = (y3 - y0 + h * y3) / H, f = y0;
    return "matrix3d(" + [a, d, 0, g / W, b, e, 0, h / H, 0, 0, 1, 0, c, f, 0, 1].join(",") + ")";
  }

  function tick(now) {
    var dt = Math.min(0.033, (now - wob.prev) / 1000);
    wob.prev = now;
    var W = innerWidth, H = innerHeight;

    wob.s += ((wob.dragging ? SCALE : 1) - wob.s) * Math.min(1, dt * 14);
    // The warp may only use the transparent margin the shrink opened up.
    var mx = (1 - wob.s) * W / 2 + 2;
    var my = (1 - wob.s) * H / 2 + 2;

    var energy = Math.abs(1 - wob.s);
    var quad = [];
    for (var i = 0; i < 4; i++) {
      var c = wob.c[i];
      c[2] += (-K * c[0] - DAMP * c[2]) * dt;
      c[3] += (-K * c[1] - DAMP * c[3]) * dt;
      c[0] = Math.max(-mx, Math.min(mx, c[0] + c[2] * dt));
      c[1] = Math.max(-my, Math.min(my, c[1] + c[3] * dt));
      energy += Math.abs(c[0]) + Math.abs(c[1]) + Math.abs(c[2]) * 0.05 + Math.abs(c[3]) * 0.05;
      quad.push([
        W / 2 + (CORNERS[i][0] * W - W / 2) * wob.s + c[0],
        H / 2 + (CORNERS[i][1] * H - H / 2) * wob.s + c[1]
      ]);
    }

    if (!wob.dragging && energy < 0.05) {
      wob.running = false;
      wob.s = 1;
      for (var j = 0; j < 4; j++) wob.c[j] = [0, 0, 0, 0];
      app.style.transform = "";
      app.classList.remove("lifted");
      send("settled");
      return;
    }
    app.style.transform = homography(W, H, quad);
    requestAnimationFrame(tick);
  }

  // ---- calls from Rust -----------------------------------------------------

  window.__init = function (s) {
    if (typeof s.font === "number") fontSize = s.font;
    if (typeof s.wobble === "boolean") wobbleOn = s.wobble;
    if (typeof s.theme === "boolean") themeOn = s.theme;
    if (s.profiles && s.profiles.length) profiles = s.profiles;
    setLevel(typeof s.glass === "number" ? s.glass : 1, false);
    if (!order.length) newTab(0);
  };

  window.__profiles = function (list) {
    if (list && list.length) profiles = list;
    if (menu.classList.contains("open")) fillMenu();
  };

  window.__out = function (id, b64) {
    var p = panes[id];
    if (!p || !p.term) return;
    var raw = atob(b64);
    var bytes = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    // Bytes, not a string: a UTF-8 sequence split across two reads is
    // reassembled by xterm's decoder instead of turning into U+FFFD.
    p.term.write(bytes);
  };

  window.__exit = function (id) { closePane(id, false); };

  window.__fail = function (id, msg) {
    var p = panes[id];
    if (p && p.term) p.term.write("\r\n\x1b[31m" + msg + "\x1b[0m\r\n");
  };

  window.__paste = function (id, text) {
    var p = panes[id];
    if (p && p.term && text) p.term.paste(text);
  };

  window.__focus = function () {
    var p = focused();
    if (p && p.term) p.term.focus();
  };

  // ---- window chrome -------------------------------------------------------

  document.querySelector(".wbtn.close").addEventListener("click", function () { send("close"); });
  document.querySelector(".wbtn.min").addEventListener("click", function () { send("minimize"); });
  document.querySelector(".wbtn.max").addEventListener("click", function () { send("maximize"); });
  document.getElementById("add").addEventListener("click", function () { newTab(0); });
  moreBtn.addEventListener("click", toggleMenu);
  glassBtn.addEventListener("click", function () { setLevel(level + 1, true); });

  // Deliberately no dblclick-to-maximize: drag_window() takes over the mouse
  // on mousedown, so two quick drags would surface as a dblclick.
  document.getElementById("drag").addEventListener("mousedown", function (e) {
    if (e.button !== 0) return;
    e.preventDefault();
    dragStart(e);
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
    resizeTimer = setTimeout(scheduleFit, 30);
  }, { passive: true });

  setLevel(level, false);
  send("ready");
})();
