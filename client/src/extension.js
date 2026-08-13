// Thin VS Code client: spawn the Rust server, speak LSP, and paint resolvable
// references so you can see what's clickable without hunting for it.

const path = require("path");
const fs = require("fs");
const vscode = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

// Teal + dotted underline for file/role/module references distinct from the theme's
// YAML value colours.
const linkDecoration = vscode.window.createTextEditorDecorationType({
  color: "#00BFA5",
  textDecoration: "underline dotted 1px",
});

// Variables get their own colour (violet) so they read as a different kind of link.
const varDecoration = vscode.window.createTextEditorDecorationType({
  color: "#B388FF",
  textDecoration: "underline dotted 1px",
});

function serverPath(context) {
  const configured = vscode.workspace
    .getConfiguration("ansibleLsp")
    .get("serverPath");
  return (
    configured ||
    context.asAbsolutePath(
      path.join(
        "..",
        "target",
        "release",
        process.platform === "win32" ? "ansible-lsp.exe" : "ansible-lsp"
      )
    )
  );
}

// The inventory actually in force — the local pick if there is one, else the committed
// setting. Module-level because `hintSettings` is called from outside `activate`, where the
// workspaceState handle does not reach; `activate` keeps it current.
let effectiveInventoryPaths = [];

// Sent at startup and again on change. Normalised here so the server sees one shape
// rather than having to know VS Code's nesting.
function hintSettings() {
  const c = vscode.workspace.getConfiguration("ansibleLsp");
  return {
    inlayHints: { enabled: c.get("inlayHints.enabled", true) },
    hover: {
      candidatesOnResolved: c.get("hover.candidatesOnResolved", false),
    },
    scan: { concurrency: c.get("scan.concurrency", 0) },
    ansiblePath: c.get("ansiblePath", ""),
    // T-062: stands in for `-i`. Empty means "model a plain ansible-playbook". Not read
    // straight from config: a local pick (workspaceState, uncommitted) outranks it.
    inventory: effectiveInventoryPaths,
  };
}

function isYaml(doc) {
  return (
    doc &&
    (doc.languageId === "ansible" || doc.languageId === "yaml") &&
    doc.uri.scheme === "file"
  );
}

// Ask the server which references resolve, and colour exactly those.
//
// Deliberately not documentLink: the server only emits links for single-target
// references, because a link's target overrides the definition provider on
// Cmd+click and would swallow the extra candidates of a templated path.
async function paint(editor) {
  if (!editor || !isYaml(editor.document) || !client?.isRunning()) return;
  try {
    const refs = await client.sendRequest("ansible/references", {
      uri: editor.document.uri.toString(),
    });
    const toDeco = (r) => ({
      range: new vscode.Range(
        r.range.start.line,
        r.range.start.character,
        r.range.end.line,
        r.range.end.character
      ),
      hoverMessage:
        r.targets > 1
          ? `${r.targets} ${r.kind === "variable" ? "definitions" : "possible targets"}`
          : undefined,
    });
    // Variables and references paint from separate lists so each keeps its own colour.
    const isVar = (r) => r.kind === "variable";
    editor.setDecorations(
      linkDecoration,
      (refs || []).filter((r) => !isVar(r)).map(toDeco)
    );
    editor.setDecorations(
      varDecoration,
      (refs || []).filter(isVar).map(toDeco)
    );
  } catch {
    // Server restarting or document closed; the next edit repaints.
  }
}

// The inventory panel's markup. Uses VS Code's own theme variables throughout, so it
// inherits the user's colours instead of inventing a second palette in the middle of the
// editor. A nonce-locked CSP because `enableScripts` is on.
//
// Two lists, not a checklist. A checkbox answers "on or off" and says nothing about
// sequence — but several inventories merge, and file order breaks ties at the same
// precedence level, so the selection is an ordered list and is drawn as one. The earlier
// checkbox versions kept reading as broken for exactly that reason: the row you ticked
// stayed where it was, and its position was load-bearing.
//
// `candidates` are the server's, not ours: each is `{path, dir, reads?}`, and a folder's
// `reads` comes from the same function that later loads it. Deriving that list here in JS
// would put one copy of ansible's directory rules in the panel and another in the reader,
// and the panel would eventually advertise a file the reader drops.
function inventoryHtml(webview, candidates, current, dest, autoSource, autoResolved) {
  const nonce = String(Math.random()).slice(2) + String(Date.now());
  // `<` escaped: a workspace path containing `</script>` would otherwise close the tag and
  // run whatever followed. JSON.stringify does not escape it, and the paths come from the
  // filesystem rather than from us.
  const data = JSON.stringify({ candidates, current, dest, autoSource, autoResolved }).replace(/</g, "\\u003c");
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy"
      content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';">
<style>
  body {
    font-family: var(--vscode-font-family);
    font-size: var(--vscode-font-size);
    color: var(--vscode-foreground);
    background: var(--vscode-editor-background);
    padding: 16px 20px 28px; margin: 0;
    max-width: 720px;
  }
  h2 { font-size: 1.15em; font-weight: 600; margin: 0 0 4px; }
  h3 {
    font-size: .78em; font-weight: 600; letter-spacing: .08em; text-transform: uppercase;
    color: var(--vscode-descriptionForeground); margin: 22px 0 6px;
  }
  .sub { color: var(--vscode-descriptionForeground); }
  ul { list-style: none; padding: 0; margin: 0; }
  li {
    display: flex; flex-direction: column; align-items: stretch; gap: 2px;
    padding: 6px 8px; border: 1px solid transparent; border-radius: 4px;
    margin-bottom: 3px;
  }
  .top { display: flex; align-items: center; gap: 8px; }
  ol.detail {
    color: var(--vscode-descriptionForeground); font-size: .85em;
    font-family: var(--vscode-editor-font-family);
    margin: 2px 0 2px 30px; padding: 0 0 0 16px;
  }
  ol.detail li {
    display: list-item; padding: 1px 4px; margin: 0; border: 1px solid transparent;
    background: none; border-radius: 3px;
  }
  ol.detail li.live { cursor: grab; }
  ol.detail li.live:hover { background: var(--vscode-list-hoverBackground); }
  ol.detail li.drag { opacity: .35; }
  ol.detail li.over { border-color: var(--vscode-focusBorder); }
  .tip {
    position: fixed; z-index: 10; max-width: 320px;
    background: var(--vscode-editorHoverWidget-background, var(--vscode-editor-background));
    color: var(--vscode-editorHoverWidget-foreground, var(--vscode-foreground));
    border: 1px solid var(--vscode-editorHoverWidget-border, var(--vscode-panel-border));
    border-radius: 4px; padding: 6px 9px; font-size: .9em; pointer-events: none;
    box-shadow: 0 2px 8px rgba(0,0,0,.35);
  }
  .hide { display: none; }
  .count {
    font-size: .8em; color: var(--vscode-descriptionForeground);
    border: 1px solid var(--vscode-panel-border); border-radius: 8px; padding: 0 6px;
  }
  li.pick { background: var(--vscode-list-inactiveSelectionBackground); }
  li.pick.drag { opacity: .35; }
  li.pick.over { border-color: var(--vscode-focusBorder); }
  li.add { cursor: pointer; }
  li.add:hover { background: var(--vscode-list-hoverBackground); }
  li.empty {
    color: var(--vscode-descriptionForeground); font-style: italic;
    border: 1px dashed var(--vscode-panel-border); align-items: center; padding: 12px;
  }
  .grip { color: var(--vscode-descriptionForeground); cursor: grab; user-select: none; }
  .name { flex: 1; font-family: var(--vscode-editor-font-family); }
  .dir { color: var(--vscode-descriptionForeground); }
  .ord {
    min-width: 1.5em; text-align: center; border-radius: 9px; font-size: .82em;
    background: var(--vscode-badge-background); color: var(--vscode-badge-foreground);
  }
  .plus { color: var(--vscode-descriptionForeground); }
  li.add:hover .plus { color: var(--vscode-foreground); }
  .icon {
    background: none; color: var(--vscode-descriptionForeground);
    padding: 3px 8px; font-size: 1.05em; line-height: 1.2; border-radius: 3px;
    min-width: 26px;
  }
  .icon:hover:not(:disabled) {
    background: var(--vscode-toolbar-hoverBackground); color: var(--vscode-foreground);
  }
  .icon:disabled { opacity: .25; cursor: default; }
  .note {
    color: var(--vscode-descriptionForeground); font-size: .92em;
    border-left: 2px solid var(--vscode-focusBorder); padding-left: 10px; margin: 10px 0 0;
  }
  .cmd {
    font-family: var(--vscode-editor-font-family); font-size: .9em;
    background: var(--vscode-textCodeBlock-background);
    color: var(--vscode-descriptionForeground);
    padding: 7px 10px; border-radius: 4px; margin: 10px 0 0;
    overflow-x: auto; white-space: pre;
  }
  .row { display: flex; gap: 8px; align-items: center; margin-top: 10px; flex-wrap: wrap; }
  .row.actions { margin-top: 22px; }
  button {
    font-family: inherit; font-size: inherit; padding: 5px 14px; border: none;
    border-radius: 3px; cursor: pointer;
    background: var(--vscode-button-background); color: var(--vscode-button-foreground);
  }
  button:hover { background: var(--vscode-button-hoverBackground); }
  button.alt {
    background: var(--vscode-button-secondaryBackground);
    color: var(--vscode-button-secondaryForeground);
  }
  button.alt:hover { background: var(--vscode-button-secondaryHoverBackground); }
  label.dest { display: flex; align-items: center; gap: 6px; cursor: pointer; padding: 2px 0; }
  fieldset { border: none; padding: 0; margin: 0; }
</style>
</head>
<body>
  <h2>Ansible inventory</h2>
  <div class="sub">Stands in for <code>-i</code>, which the editor cannot see. Decides what
    variable hover and go-to-definition read.</div>

  <h3>Read in this order</h3>
  <ul id="sel"></ul>
  <div class="note hide" id="note"></div>
  <pre class="cmd hide" id="cmd"></pre>

  <h3>Available in this workspace</h3>
  <ul id="avail"></ul>
  <div class="row"><button class="alt" id="browse">Add a file or folder…</button></div>

  <h3>Save where</h3>
  <fieldset>
    <label class="dest"><input type="radio" name="dest" value="local"> Just for me — stays on this machine, never written to the repo</label>
    <label class="dest"><input type="radio" name="dest" value="shared"> <code>.vscode/settings.json</code> — committed, shared with the team</label>
  </fieldset>

  <div class="row actions">
    <button id="save">Save</button>
    <button class="alt" id="cancel">Cancel</button>
  </div>

<script nonce="${nonce}">
const vscode = acquireVsCodeApi();
const state = ${data};
const meta = new Map(state.candidates.map(c => [c.path, c]));
// The offer order is the server's — folders first, then files, each sorted. The
// available list is
// recomputed from it on every render, so removing a row puts it back where it belongs
// instead of at the end.
let order = state.candidates.map(c => c.path);
let sel = state.current.slice();
for (const p of sel) if (order.indexOf(p) < 0) order.push(p);
const info = (p) => meta.get(p) || { path: p, dir: false };

const selList = document.getElementById("sel");
const availList = document.getElementById("avail");
const note = document.getElementById("note");
const cmd = document.getElementById("cmd");
let dragging = null;
let subDragging = null;

// Directory dimmed, name in full strength: the paths are long and the tail is the part you
// actually read. A folder keeps a trailing slash — it is a different kind of thing, and
// "reads everything inside" is the difference.
function label(p) {
  const c = info(p);
  const shown = c.dir ? p + "/" : p;
  const cut = shown.lastIndexOf("/", shown.length - 2);
  const span = document.createElement("span");
  span.className = "name";
  if (cut >= 0) {
    const dir = document.createElement("span");
    dir.className = "dir";
    dir.textContent = shown.slice(0, cut + 1);
    span.appendChild(dir);
  }
  span.appendChild(document.createTextNode(shown.slice(cut + 1)));
  return span;
}

const base = (f) => f.slice(f.lastIndexOf("/") + 1);

// Folders whose sublist is showing, and folders whose sublist has been REORDERED.
//
// The second one decides what gets saved. Untouched, a folder stays one source and one
// one -i naming the folder — ansible reads it in name order and that order is not ours
// to change. Touch
// it and the only honest way to keep what you asked for is to stop passing the folder and
// pass its files instead, so that is what happens, without a mode switch to find.
const opened = new Set();
const custom = new Map();

function filesOf(p) {
  return custom.get(p) || (info(p).reads || []).slice();
}

// Set a folder's internal order, and drop the flag when the order is back to ansible's own.
// Comparing against the natural order rather than just recording "was touched" is the
// difference between a flag and a fact: reordering and undoing it leaves the folder exactly
// as it was, so it must save as the folder again.
function setFolderOrder(p, next) {
  const natural = info(p).reads || [];
  if (next.length === natural.length && next.every((f, k) => f === natural[k])) {
    custom.delete(p);
  } else {
    custom.set(p, next);
  }
  render();
}

function reorder(list, from, to) {
  const next = list.slice();
  next.splice(to, 0, next.splice(from, 1)[0]);
  return next;
}

// What actually gets saved: a reordered folder contributes its files, everything else
// contributes itself.
function effective() {
  const out = [];
  for (const p of sel) out.push(...(custom.has(p) ? custom.get(p) : [p]));
  return out;
}

// A folder's files, numbered in the order ansible reads them. The live flag adds the
// controls that
// make the order yours — and taking it is what splits the folder.
function detail(p, live) {
  const c = info(p);
  if (!c.dir || !c.reads) return null;
  const files = filesOf(p);
  const ol = document.createElement("ol");
  ol.className = "detail";
  if (!files.length) {
    const li = document.createElement("li");
    li.textContent = "(nothing ansible would read)";
    ol.appendChild(li);
    return ol;
  }
  files.forEach((f, j) => {
    const li = document.createElement("li");
    li.appendChild(document.createTextNode(base(f)));
    if (!live) {
      ol.appendChild(li);
      return;
    }
    li.className = "live";
    li.append(
      iconButton("↑", "Read earlier", j === 0, () => setFolderOrder(p, reorder(files, j, j - 1))),
      iconButton("↓", "Read later", j === files.length - 1,
        () => setFolderOrder(p, reorder(files, j, j + 1)))
    );
    // Nested drag. The draggable flag is the load-bearing line — without it the row cannot
    // be dragged at all and only the buttons work, which is how this shipped first.
    //
    // The stopPropagation calls are defensive rather than necessary: a child's drop always
    // lands inside its own parent row, so the outer handler's own dragging-is-this-row
    // guard already returns. Tried to write a test that fails without them and could not,
    // which is the honest reason there isn't one. They stay because they keep the outer row
    // from painting itself mid-drag, and because the guard they lean on is not theirs.
    li.draggable = true;
    li.addEventListener("dragstart", (e) => {
      e.stopPropagation();
      subDragging = j;
      li.classList.add("drag");
    });
    li.addEventListener("dragend", (e) => {
      e.stopPropagation();
      subDragging = null;
      render();
    });
    li.addEventListener("dragover", (e) => {
      e.preventDefault();
      e.stopPropagation();
      li.classList.add("over");
    });
    li.addEventListener("dragleave", (e) => {
      e.stopPropagation();
      li.classList.remove("over");
    });
    li.addEventListener("drop", (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (subDragging === null || subDragging === j) return;
      const from = subDragging;
      subDragging = null;
      setFolderOrder(p, reorder(files, from, j));
    });
    ol.appendChild(li);
  });
  return ol;
}

// One tooltip element, moved and filled on hover.
//
// Not the title attribute: that is the browser's, it decides when and whether to appear,
// there is no affordance that an element has one, and it cannot be styled to match the
// editor. This version is ours, which also means it can be tested — "does the hover work"
// stopped being answerable by reading the code.
const tipEl = document.createElement("div");
tipEl.className = "tip hide";
document.body.appendChild(tipEl);

function tip(el, text) {
  if (!text) return el;
  el.dataset.tip = text;
  const place = (e) => {
    tipEl.style.left = Math.min(e.clientX + 14, window.innerWidth - 340) + "px";
    tipEl.style.top = e.clientY + 18 + "px";
  };
  el.addEventListener("mouseenter", (e) => {
    tipEl.textContent = text;
    tipEl.classList.remove("hide");
    place(e);
  });
  el.addEventListener("mousemove", place);
  el.addEventListener("mouseleave", () => tipEl.classList.add("hide"));
  return el;
}

function iconButton(glyph, hint, disabled, onClick) {
  const b = document.createElement("button");
  b.className = "icon";
  b.textContent = glyph;
  b.disabled = !!disabled;
  b.addEventListener("click", (e) => { e.stopPropagation(); onClick(); });
  return tip(b, hint);
}

function empty(list, text, hint) {
  const li = document.createElement("li");
  li.className = "empty";
  li.textContent = text;
  list.appendChild(tip(li, hint));
}

function move(from, to) {
  if (to < 0 || to >= sel.length) return;
  const moved = sel.splice(from, 1)[0];
  sel.splice(to, 0, moved);
  render();
}

function render() {
  selList.innerHTML = "";
  if (!sel.length) {
    // Whether the default finds anything is the one bit that matters, so it is the one bit
    // in the box. Which rung and which paths are a hover away — naming the rung reads like
    // something is configured, when the usual truth is that nothing is read at all.
    const auto = state.autoResolved || [];
    // No tooltip here on purpose: the note below already says which rung answered, and a
    // hover repeating a line that is on screen two centimetres away is just noise.
    empty(selList, auto.length
      ? "Nothing chosen — " + state.autoSource + " reads " + auto.join(", ")
      : "Nothing chosen, and no default here — no inventory is read");
  }
  sel.forEach((p, i) => {
    const li = document.createElement("li");
    li.className = "pick";
    li.draggable = true;
    const grip = document.createElement("span");
    grip.className = "grip";
    grip.textContent = "⠿";
    const ord = document.createElement("span");
    ord.className = "ord";
    ord.textContent = String(i + 1);
    const top = document.createElement("div");
    top.className = "top";
    const c = info(p);
    top.append(
      grip, ord, label(p),
      iconButton("↑", "Read earlier", i === 0, () => move(i, i - 1)),
      iconButton("↓", "Read later", i === sel.length - 1, () => move(i, i + 1))
    );
    if (c.dir && c.reads && c.reads.length > 1) {
      const isOpen = opened.has(p);
      top.appendChild(iconButton(
        isOpen ? "▾" : "▸",
        isOpen ? "Hide the files inside" : "Show the files inside, and reorder them",
        false,
        () => {
          if (isOpen) opened.delete(p);
          else opened.add(p);
          render();
        }
      ));
    }
    top.appendChild(iconButton("✕", "Remove", false, () => {
      sel.splice(i, 1);
      custom.delete(p);
      opened.delete(p);
      render();
    }));
    if (custom.has(p)) {
      const tag = document.createElement("span");
      tag.className = "count";
      tag.textContent = "your order";
      top.appendChild(tip(tag,
        "Saved as separate -i entries, since a folder's own order is ansible's"));
    }
    li.appendChild(top);
    if (opened.has(p)) {
      const d = detail(p, true);
      if (d) li.appendChild(d);
    }
    li.addEventListener("dragstart", () => { dragging = i; li.classList.add("drag"); });
    li.addEventListener("dragend", () => { dragging = null; render(); });
    li.addEventListener("dragover", (e) => { e.preventDefault(); li.classList.add("over"); });
    li.addEventListener("dragleave", () => li.classList.remove("over"));
    li.addEventListener("drop", (e) => {
      e.preventDefault();
      if (dragging === null || dragging === i) return;
      move(dragging, i);
      dragging = null;
    });
    selList.appendChild(li);
  });

  availList.innerHTML = "";
  const avail = order.filter(p => sel.indexOf(p) < 0);
  if (!avail.length) {
    empty(availList, sel.length
      ? "Everything found is in the list above."
      : "Nothing here reads as an inventory. Use “Add a file or folder…”.");
  }
  avail.forEach((p) => {
    const c = info(p);
    const li = document.createElement("li");
    li.className = "add";
    tip(li, c.dir
      ? p + " — a folder, read whole: " + (c.reads || []).length + " files"
      : p);
    const plus = document.createElement("span");
    plus.className = "plus";
    plus.textContent = "+";
    const top = document.createElement("div");
    top.className = "top";
    top.append(plus, label(p));
    if (c.dir && c.reads) {
      const n = document.createElement("span");
      n.className = "count";
      n.textContent = c.reads.length + (c.reads.length === 1 ? " file" : " files");
      top.appendChild(n);
    }
    li.appendChild(top);
    const d = detail(p, false);
    if (d) li.appendChild(d);
    // The whole row, not a small target inside it: the checkbox versions failed here.
    li.addEventListener("click", () => {
      sel.push(p);
      render();
    });
    availList.appendChild(li);
  });

  const one = sel.length === 1 ? info(sel[0]) : null;
  if (sel.length > 1) {
    note.textContent = "Merged top to bottom. On a name collision Ansible's group " +
      "precedence decides first — a host beats a named group, a named group beats all — " +
      "and only at the same level does the LATER file win.";
    note.classList.remove("hide");
  } else if (one && one.dir) {
    note.textContent = custom.has(sel[0])
      ? "You reordered inside the folder, so it is saved as its files rather than as the " +
        "folder — a folder's own order is ansible's (name order) and cannot be changed."
      : "One folder, read whole, in the name order shown. Press ▸ to see the files and " +
        "reorder them; do that and it is saved as the files instead of the folder.";
    note.classList.remove("hide");
  } else if (sel.length === 1) {
    note.textContent = "One inventory file, so nothing to merge and order does not matter.";
    note.classList.remove("hide");
  } else if ((state.autoResolved || []).length) {
    note.textContent = "That is " + state.autoSource + "'s answer. Pick one above to " +
      "override it, the way -i does.";
    note.classList.remove("hide");
  } else {
    // Shown, not hovered. Which rung answered is the question the empty box raises, and an
    // answer you have to discover by hovering is one most people never see.
    note.textContent = "Ansible reads -i, then ANSIBLE_INVENTORY, then ansible.cfg, then " +
      "/etc/ansible/hosts. Here it falls through to " + state.autoSource + ", which is " +
      "not there.";
    note.classList.remove("hide");
  }

  const eff = effective();
  if (eff.length) {
    cmd.textContent = "ansible-playbook " +
      eff.map(p => "-i " + (/\\s/.test(p) ? JSON.stringify(p) : p)).join(" ") + " …";
    cmd.classList.remove("hide");
  } else {
    cmd.classList.add("hide");
  }
}

document.getElementById("browse").addEventListener("click",
  () => vscode.postMessage({ type: "browse" }));
document.querySelector('input[value="' + (state.dest === "shared" ? "shared" : "local") + '"]').checked = true;
document.getElementById("save").addEventListener("click", () => {
  vscode.postMessage({
    type: "save",
    paths: effective(),
    dest: document.querySelector('input[name=dest]:checked').value,
  });
});
document.getElementById("cancel").addEventListener("click",
  () => vscode.postMessage({ type: "cancel" }));
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") vscode.postMessage({ type: "cancel" });
});
// A browsed path was chosen deliberately, so it lands selected rather than merely offered.
// It arrives as {path, dir} because only the host can stat it; without that flag a browsed
// folder would render as a file and claim an order that does not apply to it.
window.addEventListener("message", (e) => {
  if (e.data.type !== "add") return;
  for (const c of e.data.paths) {
    if (!meta.has(c.path)) meta.set(c.path, c);
    if (order.indexOf(c.path) < 0) order.push(c.path);
    if (sel.indexOf(c.path) < 0) sel.push(c.path);
  }
  render();
});
render();
</script>
</body>
</html>`;
}

function activate(context) {
  const command = serverPath(context);

  if (!fs.existsSync(command)) {
    vscode.window.showErrorMessage(
      `ansible-lsp binary not found at ${command}. Run \`cargo build --release\`, or set ansibleLsp.serverPath.`
    );
    return;
  }

  client = new LanguageClient(
    "ansibleLsp",
    "Ansible LSP",
    { command, transport: TransportKind.stdio },
    {
      documentSelector: [
        { scheme: "file", language: "ansible" },
        { scheme: "file", language: "yaml" },
      ],
      // A function, not a value: evaluated on every (re)start, so `ansibleLsp.restart`
      // picks up current settings instead of replaying the activation-time snapshot.
      initializationOptions: hintSettings,
    }
  );

  const timers = new Map();
  const repaint = (editor) => {
    if (!editor) return;
    const key = editor.document.uri.toString();
    clearTimeout(timers.get(key));
    timers.set(key, setTimeout(() => paint(editor), 250));
  };

  context.subscriptions.push(
    linkDecoration,
    varDecoration,
    // Picks up a `cargo build --release` without reloading the whole window.
    vscode.commands.registerCommand("ansibleLsp.restart", async () => {
      await client.restart();
      vscode.window.visibleTextEditors.forEach(repaint);
      vscode.window.setStatusBarMessage("Ansible LSP restarted", 2000);
    }),
    // Settings take effect immediately; the server asks VS Code to re-request hints.
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (
        !e.affectsConfiguration("ansibleLsp.inlayHints") &&
        !e.affectsConfiguration("ansibleLsp.hover") &&
        !e.affectsConfiguration("ansibleLsp.scan") &&
        !e.affectsConfiguration("ansibleLsp.inventory")
      )
        return;
      client?.sendNotification("workspace/didChangeConfiguration", {
        settings: hintSettings(),
      });
      if (e.affectsConfiguration("ansibleLsp.inventory")) {
        // Primes `effectiveInventoryPaths` before `client.start()` (below) evaluates
  // `initializationOptions`, so the server's first view already includes a local pick.
  // Cannot move earlier: `INV_KEY` above is a `const`, so touching it sooner is a TDZ error.
  effectiveInventory();
        paintInventory(lastInventoryInfo);
      }
    }),
    vscode.window.onDidChangeActiveTextEditor(repaint),
    vscode.window.onDidChangeVisibleTextEditors((es) => es.forEach(repaint)),
    vscode.workspace.onDidChangeTextDocument((e) => {
      for (const editor of vscode.window.visibleTextEditors) {
        if (editor.document === e.document) repaint(editor);
      }
    })
  );

  // Persistent "is Ansible installed" indicator. A startup toast auto-dismisses and gets
  // buried under VS Code's own notifications; a status-bar item stays until it's fixed.
  const ansibleStatus = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    0
  );
  context.subscriptions.push(ansibleStatus);
  // Registered before start() so the `ansible/status` the server sends during `initialized`
  // isn't missed.
  client.onNotification("ansible/status", (p) => {
    if (p && p.found === false) {
      ansibleStatus.text = "$(warning) Ansible: not found";
      ansibleStatus.tooltip =
        "ansible isn't on PATH, so builtin modules (ansible.builtin.*) and installed " +
        "collections can't resolve. Files, roles, and in-repo modules still work. " +
        "Install ansible-core (WSL on Windows).";
      ansibleStatus.show();
    } else {
      ansibleStatus.hide();
    }
  });

  // T-062: which inventory is in effect. The ticket exists because "which one?" is
  // ambiguous — three inventories in a repo can disagree about the same variable — so a
  // server that picks one silently reproduces the problem. Show the answer, and make
  // switching cheap.
  //
  // Two stores, deliberately:
  //   workspaceState  your own `-i`, per machine, NEVER written into the repo
  //   ansibleLsp.inventory  a committed project default, for a team that shares one
  // Yours wins. Writing to either clears the other, so "which one is in force" is never a
  // puzzle — a shadowed setting that silently does nothing is the failure mode to avoid.
  const INV_KEY = "ansibleLsp.inventory.local";
  const INV_DEST = "ansibleLsp.inventory.dest";

  function localInventory() {
    return context.workspaceState.get(INV_KEY, []);
  }
  function sharedInventory() {
    return vscode.workspace.getConfiguration("ansibleLsp").get("inventory", []);
  }
  function effectiveInventory() {
    const local = localInventory();
    const eff = local.length ? local : sharedInventory();
    effectiveInventoryPaths = eff;
    return eff;
  }
  // Primes `effectiveInventoryPaths` before `client.start()` (below) evaluates
  // `initializationOptions`, so the server's first view already includes a local pick.
  // Cannot move earlier: `INV_KEY` above is a `const`, so touching it sooner is a TDZ error.
  effectiveInventory();

  const inventoryStatus = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    0
  );
  inventoryStatus.command = "ansibleLsp.pickInventory";
  context.subscriptions.push(inventoryStatus);

  let lastInventoryInfo = null;

  // Where the automatic choice came from, as the server reported it.
  function autoSource() {
    return (lastInventoryInfo && lastInventoryInfo.autoSource) || "/etc/ansible/hosts";
  }

  // Shown from activation, not from a server notification: a control that appears only
  // once the server speaks leaves you with no way to set `-i` on exactly the workspaces
  // where the answer is unclear.
  function paintInventory(info) {
    const chosen = effectiveInventory();
    const where = localInventory().length ? "this machine" : "settings.json";
    const resolved = (info && info.resolved) || [];
    if (chosen.length) {
      const first = chosen[0].split("/").pop();
      inventoryStatus.text =
        chosen.length > 1
          ? `$(list-tree) ${first} +${chosen.length - 1}`
          : `$(list-tree) ${first}`;
      inventoryStatus.tooltip =
        `Inventory (from ${where}) — stands in for \`-i\`:\n` +
        chosen.join("\n") +
        (chosen.length > 1
          ? "\n\nMerged. On a collision Ansible's group precedence decides first (a host " +
            "beats a named group, a named group beats `all`); only at the same level does " +
            "the later file win."
          : "") +
        "\n\nClick to change. This is what variable hover and go-to-definition read.";
    } else {
      inventoryStatus.text = "$(list-tree) inventory: auto";
      inventoryStatus.tooltip =
        `No inventory chosen — following ${autoSource()}, as a plain \`ansible-playbook\` ` +
        "would.\n\nClick to name the `-i` you run with. Variables defined only in an " +
        "inventory can't resolve until one is set.";
    }
    inventoryStatus.show();
  }
  paintInventory(null);

  client.onNotification("ansible/inventory", (p) => {
    lastInventoryInfo = p;
    paintInventory(p);
  });

  // One panel at a time: a second copy would open showing the saved state while the first
  // still holds unsaved edits, and whichever you pressed Save on last would win.
  let invPanel = null;

  context.subscriptions.push(
    vscode.commands.registerCommand("ansibleLsp.pickInventory", async () => {
      if (invPanel) {
        invPanel.reveal();
        return;
      }
      // The server's offer, not ours: it sniffs content rather than matching names, so it
      // finds `prod/db.ini` that no name pattern would, and it can expand a folder with the
      // same code that later reads it. `findFiles` is the fallback for the window between
      // activation and the server's first notification.
      let candidates = (lastInventoryInfo && lastInventoryInfo.candidates) || null;
      if (!candidates) {
        const found = await vscode.workspace.findFiles(
          "**/{inventory,inventories,hosts}*",
          "**/{node_modules,.git}/**",
          50
        );
        candidates = found
          .map((u) => vscode.workspace.asRelativePath(u))
          .filter((p) => !p.includes("/group_vars/") && !p.includes("/host_vars/"))
          .sort()
          .map((path) => ({ path, dir: false }));
      }
      const current = effectiveInventory();
      const known = [
        ...candidates,
        ...current
          .filter((p) => !candidates.some((c) => c.path === p))
          .map((path) => ({ path, dir: false })),
      ];

      // A webview, not a quick pick. Two things a quick pick structurally cannot do, and
      // both matter here:
      //   ORDER — several inventories merge, and file order breaks ties at the same
      //           precedence level, so the sequence is semantic. `canPickMany` returns
      //           items in list order, never the order you clicked.
      //   the toggle-all control `canPickMany` adds beside the filter box, which sits
      //           where the first row looks like it should be and selects everything.
      const panel = vscode.window.createWebviewPanel(
        "ansibleLspInventory",
        "Ansible inventory",
        vscode.ViewColumn.Active,
        { enableScripts: true, retainContextWhenHidden: true }
      );
      invPanel = panel;
      panel.onDidDispose(() => {
        invPanel = null;
      });
      panel.webview.html = inventoryHtml(
        panel.webview,
        known,
        current,
        context.workspaceState.get(INV_DEST, "local"),
        autoSource(),
        (lastInventoryInfo && lastInventoryInfo.autoResolved) || []
      );
      panel.webview.onDidReceiveMessage(async (m) => {
        if (m.type === "browse") {
          const chosen = await vscode.window.showOpenDialog({
            canSelectMany: true,
            canSelectFolders: true,
            canSelectFiles: true,
            openLabel: "Use as inventory",
          });
          if (chosen && chosen.length) {
            // Stat rather than guess from the name: a folder with a dotted name is common
            // (`inventories/prod.d`), and a file with none is too.
            const paths = await Promise.all(
              chosen.map(async (u) => ({
                path: vscode.workspace.asRelativePath(u),
                dir: (await vscode.workspace.fs.stat(u)).type ===
                  vscode.FileType.Directory,
              }))
            );
            panel.webview.postMessage({ type: "add", paths });
          }
          return;
        }
        if (m.type === "cancel") {
          panel.dispose();
          return;
        }
        if (m.type !== "save") return;
        const picked = m.paths || [];
        const cfg = vscode.workspace.getConfiguration("ansibleLsp");
        if (m.dest === "shared") {
          // Cleared, or the local pick would shadow what was just written and the setting
          // would appear to do nothing.
          await context.workspaceState.update(INV_KEY, []);
          await context.workspaceState.update(INV_DEST, "shared");
          await cfg.update("inventory", picked, vscode.ConfigurationTarget.Workspace);
        } else {
          await context.workspaceState.update(INV_KEY, picked);
          await context.workspaceState.update(INV_DEST, "local");
        }
        paintInventory(lastInventoryInfo);
        client?.sendNotification("workspace/didChangeConfiguration", {
          settings: hintSettings(),
        });
        // The panel closes on save, so without this the only trace is a status-bar item you
        // weren't looking at.
        vscode.window.setStatusBarMessage(
          picked.length
            ? `Inventory: ${picked.map((p) => p.split("/").pop()).join(", ")}`
            : "Inventory: automatic",
          3000
        );
        panel.dispose();
      });
    })
  );

  // Not awaited: activate() must return promptly so the extension host isn't blocked.
  client.start().then(() => vscode.window.visibleTextEditors.forEach(repaint));
}

function deactivate() {
  return client?.stop();
}

module.exports = { activate, deactivate };
