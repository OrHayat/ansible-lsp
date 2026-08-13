// A DOM stub just big enough to RUN the inventory panel's script.
//
// The markup checks in webview.js only parse that script. Everything that has actually
// broken in this panel — a stuck flag, a row that would not drag, a click target that did
// nothing — lives in its behaviour, and behaviour needs the code to run. This is the
// smallest thing that lets it.
//
// It implements only what the panel uses. A method the panel does not call is absent on
// purpose: a stub that quietly accepts everything would let the panel drift into APIs this
// never proves exist.

function makeEl(tag) {
  const el = {
    tagName: tag,
    children: [],
    handlers: {},
    dataset: {},
    style: {},
    title: "",
    draggable: false,
    disabled: false,
    // className and classList are two views of ONE set, as in a real DOM. They were
    // separate here, so `className = "tip hide"` followed by `classList.remove("hide")`
    // left the class in place in the stub and removed it in the browser — the stub
    // disagreeing with the thing it stands in for, which is worse than having no stub.
    classList: {
      set: new Set(),
      add(c) { this.set.add(c); },
      remove(c) { this.set.delete(c); },
      contains(c) { return this.set.has(c); },
    },
    get className() { return [...this.classList.set].join(" "); },
    set className(v) {
      this.classList.set = new Set(String(v).split(/\s+/).filter(Boolean));
    },
    appendChild(c) { c.parent = this; this.children.push(c); return c; },
    append(...cs) { for (const c of cs) c.parent = this; this.children.push(...cs); },
    addEventListener(type, fn) { (this.handlers[type] ||= []).push(fn); },
    // Events BUBBLE. Not a detail: the panel nests a draggable list inside a draggable row,
    // and the bug that made sublist rows unusable was the parent handling the child's drag.
    // A stub that did not bubble could not see that, and reported the fix as covered when
    // it was not.
    fire(type, ev) {
      let stopped = false;
      const e = Object.assign(
        { preventDefault() {}, stopPropagation() { stopped = true; } },
        ev
      );
      for (let node = this; node; node = node.parent) {
        for (const fn of node.handlers[type] || []) fn(e);
        if (stopped) return;
      }
    },
    set innerHTML(v) { if (v === "") this.children = []; },
    get innerHTML() { return ""; },
    set textContent(v) { this.text = v; this.children = []; },
    get textContent() { return this.text || ""; },
  };
  return el;
}

// Depth-first over the rendered tree.
function walk(el, out = []) {
  out.push(el);
  for (const c of el.children || []) walk(c, out);
  return out;
}

function byTip(root, fragment) {
  return walk(root).find((e) => ((e.dataset || {}).tip || "").includes(fragment));
}

function allText(el) {
  return walk(el)
    .map((e) => e.text || "")
    .join(" ");
}

// Returns { sent, els } — every postMessage the panel made, and the elements it built.
function runPanel(scriptSource) {
  const vm = require("vm");
  const sent = [];
  const els = {};
  for (const id of ["sel", "avail", "note", "cmd", "browse", "save", "cancel"]) {
    els[id] = makeEl("div");
  }
  const radios = {
    local: { value: "local", checked: false },
    shared: { value: "shared", checked: false },
  };
  els.body = makeEl("body");
  const document = {
    body: els.body,
    getElementById: (id) => els[id],
    createElement: makeEl,
    createTextNode: (t) => ({ tagName: "#text", text: t, children: [] }),
    addEventListener() {},
    querySelector(sel) {
      const m = sel.match(/value="(\w+)"/);
      if (m) return radios[m[1]];
      if (sel.includes(":checked")) {
        return Object.values(radios).find((r) => r.checked) || radios.local;
      }
      return null;
    },
  };
  const sandbox = {
    document,
    window: { addEventListener() {}, innerWidth: 800 },
    acquireVsCodeApi: () => ({ postMessage: (m) => sent.push(m) }),
  };
  vm.createContext(sandbox);
  vm.runInContext(scriptSource, sandbox, { filename: "inventory-webview.js" });
  return { sent, els, radios, byTip, allText, walk };
}

module.exports = { runPanel, walk, byTip, allText };
