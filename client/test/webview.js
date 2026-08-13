// Checks the inventory panel's markup. `node test/webview.js`, or `npm test` from client/.
//
// Everything else in the client is verified by clicking, which is why this exists: the
// panel's script is a STRING until a webview parses it, so a typo in it cannot fail a
// build, cannot fail a lint, and shows up only as a blank panel with an error in a devtools
// console nobody has open.
//
// It loads the real extension.js and calls the real inventoryHtml. Evaluating a copy of the
// function would test the copy — the mistake this repo has already made twice.

const fs = require("fs");
const path = require("path");
const vm = require("vm");
const Module = require("module");

const SRC = path.join(__dirname, "..", "src", "extension.js");

// extension.js requires `vscode` at load time, which only exists inside the extension host.
const stubs = {
  vscode: {
    window: {
      createTextEditorDecorationType: () => ({}),
      createStatusBarItem: () => ({ show() {}, hide() {} }),
    },
    workspace: { getConfiguration: () => ({ get: (_k, d) => d }) },
    StatusBarAlignment: { Left: 1 },
    ViewColumn: { Active: -1 },
    ConfigurationTarget: { Workspace: 2 },
    commands: { registerCommand: () => ({}) },
    Range: class {},
  },
  "vscode-languageclient/node": {
    LanguageClient: class {},
    TransportKind: { stdio: "stdio" },
  },
};
const load = Module._load;
Module._load = (req, parent, isMain) =>
  req in stubs ? stubs[req] : load(req, parent, isMain);

// inventoryHtml is module-private; run the file in a context and read it back out.
const sandbox = { require, module: { exports: {} }, exports: {}, console, __dirname };
vm.createContext(sandbox);
vm.runInContext(fs.readFileSync(SRC, "utf8"), sandbox);
const inventoryHtml = sandbox.inventoryHtml;

let failed = 0;
function check(name, fn) {
  try {
    fn();
    console.log("ok   " + name);
  } catch (e) {
    failed++;
    console.log("FAIL " + name + "\n       " + e.message);
  }
}
function assert(cond, msg) {
  if (!cond) throw new Error(msg);
}

const WEBVIEW = { cspSource: "vscode-webview:" };
const render = (candidates, current, dest, auto, autoResolved, configFile, hasLocal,
    shared) =>
  inventoryHtml(WEBVIEW, candidates, current, dest || "local", auto || "ansible.cfg",
    autoResolved || [], configFile || null, hasLocal === undefined ? true : hasLocal,
    shared || []);
const file = (path) => ({ path, dir: false });

// The shape the server actually sends, folders included — asserting against a list of bare
// strings would test a shape nothing produces any more.
const FOLDER = {
  path: "inventories/prod",
  dir: true,
  reads: ["inventories/prod/db.ini", "inventories/prod/hosts.ini"],
};
const html = render(
  [FOLDER, file("demo/inventory-prod.ini"), file("inventory.ini")],
  ["demo/inventory-prod.ini"]
);

function scriptOf(page) {
  const m = page.match(/<script nonce="[^"]+">([\s\S]*?)<\/script>/);
  assert(m, "no script block in the rendered page");
  return m[1];
}

check("the emitted script parses", () => {
  new vm.Script(scriptOf(html), { filename: "inventory-webview.js" });
});

check("no ${} survives into the emitted script", () => {
  assert(
    !/\$\{/.test(scriptOf(html)),
    "an interpolation was escaped by mistake and shipped as literal text"
  );
});

check("the CSP nonce is the script tag's nonce", () => {
  const csp = html.match(/nonce-([^']+)'/);
  assert(csp, "no nonce in the CSP");
  assert(
    html.includes('<script nonce="' + csp[1] + '"'),
    "CSP nonce and script nonce differ, so nothing runs"
  );
});

// A path is filesystem data, not ours. JSON.stringify does not escape `<`, so without the
// explicit escape a directory named `</script>` closes the tag and runs what follows.
check("a path cannot break out of the script tag", () => {
  const evil = render(["a/</script><img src=x onerror=alert(1)>/hosts"], []);
  assert(
    (evil.match(/<\/script>/g) || []).length === 1,
    "the page has more than one </script>, so a path closed the tag"
  );
});

check("both destinations render, and the stored one is preselected", () => {
  assert(/value="local"/.test(html) && /value="shared"/.test(html), "a destination is missing");
  const dest = (page) => (scriptOf(page).match(/"dest"\s*:\s*"(\w+)"/) || [])[1];
  assert(dest(render([], [], "shared")) === "shared", "a saved 'shared' came back as " + dest(render([], [], "shared")));
  assert(dest(render([], [], "local")) === "local", "a saved 'local' came back as " + dest(render([], [], "local")));
});

check("the selection is passed through in order, not sorted", () => {
  const s = scriptOf(render([file("b.yml"), file("a.ini")], ["b.yml", "a.ini"]));
  const cur = s.match(/"current"\s*:\s*\[([^\]]*)\]/)[1];
  assert(
    cur.indexOf('"b.yml"') < cur.indexOf('"a.ini"'),
    "the order the user chose was lost before it reached the page: " + cur
  );
});

// The folder's expansion is the server's answer, computed by the same function that reads
// it. If it stopped reaching the page the panel would show a bare folder name and no
// indication of what picking it actually loads.
check("a folder carries its file list, in read order", () => {
  const s = scriptOf(html);
  assert(/"dir"\s*:\s*true/.test(s), "the folder flag did not survive");
  const reads = s.match(/"reads"\s*:\s*\[([^\]]*)\]/)[1];
  // Name order is ansible's own and is what the panel numbers. Reversing it in the payload
  // would show a merge sequence that does not happen.
  assert(
    reads.indexOf("db.ini") < reads.indexOf("hosts.ini"),
    "the folder's read order did not survive: " + reads
  );
});

check("a folder's own path is not also offered as one of its files", () => {
  const s = scriptOf(html);
  const paths = [...s.matchAll(/"path"\s*:\s*"([^"]+)"/g)].map((m) => m[1]);
  assert(
    new Set(paths).size === paths.length,
    "a path is offered twice: " + paths.join(", ")
  );
});

// ------------------------------------------------------------------ behaviour
// From here the panel's script is RUN, not just parsed. Everything below is a bug that was
// found by clicking, which is the argument for the stub existing at all.

const { runPanel, byTip, allText, walk } = require("./dom");

function panel(candidates, current) {
  const page = render(candidates, current);
  const p = runPanel(scriptOf(page), page);
  p.save = () => {
    p.els.save.fire("click");
    return (p.sent[p.sent.length - 1] || {}).paths;
  };
  // The sublist rows, and their buttons. Addressed by structure rather than by title:
  // the outer row carries buttons with the SAME titles, and a title search finds those
  // first — which made an earlier version of these tests pass while clicking a disabled
  // outer button and changing nothing.
  p.subRows = () => walk(p.els.sel).filter((e) => e.className === "live");
  p.subButton = (row, glyph) =>
    (row.children || []).find((c) => c.tagName === "button" && c.text === glyph);
  p.open = () => byTip(p.els.sel, "Show the files inside").fire("click");
  return p;
}

const seq = (a) => JSON.stringify(a);
const PROD_DB = "inventories/prod/db.ini";
const PROD_HOSTS = "inventories/prod/hosts.ini";

check("a folder saves as the folder until its order is changed", () => {
  const p = panel([FOLDER], [FOLDER.path]);
  p.open();
  assert(
    seq(p.save()) === seq(["inventories/prod"]),
    "merely opening the sublist must not split it, got " + seq(p.save())
  );
});

check("reordering inside a folder saves its files instead", () => {
  const p = panel([FOLDER], [FOLDER.path]);
  p.open();
  p.subButton(p.subRows()[0], "\u2193").fire("click"); // db.ini down, so hosts.ini leads
  assert(
    seq(p.save()) === seq([PROD_HOSTS, PROD_DB]),
    "expected the two files in the chosen order, got " + seq(p.save())
  );
});

// The bug: the panel recorded "was touched" instead of comparing to ansible's own order, so
// moving a row and moving it back left the folder permanently split.
check("reordering back to ansible's own order saves as the folder again", () => {
  const p = panel([FOLDER], [FOLDER.path]);
  p.open();
  p.subButton(p.subRows()[0], "\u2193").fire("click");
  assert(seq(p.save()) === seq([PROD_HOSTS, PROD_DB]), "the control move did not take");
  p.subButton(p.subRows()[1], "\u2191").fire("click");
  assert(
    seq(p.save()) === seq(["inventories/prod"]),
    "undoing the reorder must restore the folder, got " + seq(p.save())
  );
  assert(
    !allText(p.els.sel).includes("your order"),
    "the 'your order' tag survived an undo"
  );
});

// The other bug: only the tiny buttons worked, because the sublist rows were never marked
// draggable and the parent row swallowed the drag.
check("sublist rows are draggable, and their drag does not move the outer row", () => {
  const p = panel([FOLDER, file("other.ini")], [FOLDER.path, "other.ini"]);
  p.open();
  const rows = p.subRows();
  assert(rows.length === 2, "expected 2 sublist rows, got " + rows.length);
  assert(rows.every((r) => r.draggable), "a sublist row was not draggable");

  rows[0].fire("dragstart");
  rows[1].fire("drop");
  assert(
    seq(p.save()) === seq([PROD_HOSTS, PROD_DB, "other.ini"]),
    "a sublist drag must reorder inside the folder only, got " + seq(p.save())
  );
});

// "following /etc/ansible/hosts" reads like something is configured. Usually nothing is —
// the file is the last resort and is normally absent — and the difference decides whether
// every inventory variable in the workspace is expected to resolve or expected not to.
check("the empty state says whether the automatic default finds anything", () => {
  const none = runPanel(scriptOf(render([], [], "local", "/etc/ansible/hosts", [])));
  const text = allText(none.els.sel);
  assert(text.includes("no inventory is read"), "a default that reads nothing must say so: " + text);

  const some = runPanel(scriptOf(render([], [], "local", "ansible.cfg", ["inventory.yml"])));
  const text2 = allText(some.els.sel);
  assert(
    text2.includes("ansible.cfg reads inventory.yml"),
    "a default that reads something must name it: " + text2
  );
  // One line, not a paragraph: this box is an empty state, and it was a wall of text.
  assert(text.length < 90 && text2.length < 90, "the empty state got long again");

  // The rung has to be VISIBLE, not hovered — it lived in a title attribute and nobody
  // found it. It belongs in the note, which is on screen, and NOT also in a tooltip: the
  // two said the same thing at once.
  assert(
    allText(none.els.note).includes("/etc/ansible/hosts"),
    "the note must name the rung that answered: " + allText(none.els.note)
  );
  assert(
    allText(some.els.note).includes("ansible.cfg"),
    "the note must name the rung that answered: " + allText(some.els.note)
  );
  // The box does carry a tooltip — the ladder — but it must not be the note again. That
  // duplication is what got the tooltip removed once, and removing it was the wrong fix.
  const boxTip = (none.els.sel.children[0].dataset || {}).tip || "";
  assert(boxTip && boxTip !== text, "the empty box repeats the note in its tooltip");
});

// The question that started this: "why didn't the hover work?" — unanswerable while the
// tooltip was the browser's `title`, since nothing in our code decided whether it appeared.
// It is ours now, so it is a test.
check("hovering shows a tooltip, and leaving hides it again", () => {
  const p = panel([FOLDER, file("demo/inventory-prod.ini")], [FOLDER.path]);
  const tipEl = p.els.body.children.find((c) => (c.className || "").includes("tip"));
  assert(tipEl, "no tooltip element was created");
  assert(tipEl.classList.contains("hide"), "the tooltip starts visible");

  const target = byTip(p.els.sel, "Read later");
  assert(target, "nothing carried a tooltip");
  target.fire("mouseenter", { clientX: 100, clientY: 200 });
  assert(!tipEl.classList.contains("hide"), "hovering did not show the tooltip");
  assert(tipEl.text.includes("Read later"), "the tooltip showed the wrong text: " + tipEl.text);
  assert(tipEl.style.left === "114px" && tipEl.style.top === "218px",
    "the tooltip did not follow the pointer: " + tipEl.style.left + "," + tipEl.style.top);

  target.fire("mouseleave");
  assert(tipEl.classList.contains("hide"), "leaving did not hide the tooltip");
});

// An available row's tooltip is its path — the rows show a shortened form, and the full one
// has to be reachable without clicking.
check("an offered row's tooltip names what it is", () => {
  const p = panel([FOLDER, file("demo/inventory-prod.ini")], []);
  assert(byTip(p.els.avail, "inventories/prod — a folder"), "the folder row lost its tooltip");
  assert(byTip(p.els.avail, "demo/inventory-prod.ini"), "the file row lost its tooltip");
});

// "ansible.cfg" is a category, not an answer. Ansible reads the one in the directory you
// run from — measured: no walk up, and a second file never merges — so in a repo with more
// than one, naming the file is the only way to notice we read a different one than you do.
check("the note names the exact rung, not its category", () => {
  const cfg = runPanel(scriptOf(
    render([], [], "local", "ansible.cfg", ["inventory.yml"], "demo/ansible.cfg")));
  assert(
    allText(cfg.els.note).includes("demo/ansible.cfg"),
    "the note must name the config file it read: " + allText(cfg.els.note)
  );

  const env = runPanel(scriptOf(
    render([], [], "local", "ANSIBLE_INVENTORY", ["inv.ini"], null)));
  assert(
    allText(env.els.note).includes("ANSIBLE_INVENTORY"),
    "an env-var answer must say so: " + allText(env.els.note)
  );

  // A config that exists but names no inventory is a different state from no config, and
  // it is the one where you go looking for a typo.
  const quiet = runPanel(scriptOf(
    render([], [], "local", "/etc/ansible/hosts", [], "ansible.cfg")));
  assert(
    allText(quiet.els.note).includes("names no inventory"),
    "a config with no inventory key must say so: " + allText(quiet.els.note)
  );
});

// The empty box was the one row in the panel with no tooltip, which read as broken rather
// than as deliberate. It has one again — but it must ADD to the note, not repeat it: the
// note says which rung answered, the hover says what every rung held.
check("the empty box has a tooltip, and it is not the note again", () => {
  const p = panel([], []);
  const box = p.els.sel.children[0];
  const hover = (box.dataset || {}).tip || "";
  assert(hover, "the empty box has no tooltip");
  for (const rung of ["-i", "ANSIBLE_INVENTORY", "ansible.cfg", "/etc/ansible/hosts"]) {
    assert(hover.includes(rung), "the ladder omits " + rung + ": " + hover);
  }
  assert(hover !== allText(p.els.note).trim(), "the tooltip is the note verbatim");
  assert(hover.split("\n").length >= 5, "the ladder collapsed to one line: " + hover);
});

// Absent, [] and [..] are three different answers. Collapsing the first two is what left
// no way to clear a pick, and no way to say "no inventory, deliberately" over a committed
// default — so the two controls that produce those states must be distinguishable.
check("saving an empty list and forgetting the pick are different actions", () => {
  const p = panel([file("a.ini")], ["a.ini"]);
  walk(p.els.sel).filter((e) => e.tagName === "button" && e.text === "\u2715")[0].fire("click");
  assert(seq(p.save()) === seq([]), "saving an emptied list must send [], got " + seq(p.save()));

  p.els.forget.fire("click");
  const last = p.sent[p.sent.length - 1];
  assert(last.type === "forget", "the forget button sent " + JSON.stringify(last));
});

// Greyed rather than hidden: a control that vanishes takes its explanation with it, and
// the label is where you read what restoring would give you.
check("the forget button greys out when there is nothing to forget, and names its target", () => {
  const live = render([], [], "local", "ansible.cfg", [], null, true, ["inventories/prod"]);
  const on = runPanel(scriptOf(live), live);
  assert(!on.els.forget.disabled, "disabled while a local pick exists");
  assert(
    on.els.forget.text.includes("prod"),
    "the button must name what it restores to, got: " + on.els.forget.text
  );

  const dead = render([], [], "local", "ansible.cfg", [], null, false, []);
  const off = runPanel(scriptOf(dead), dead);
  assert(off.els.forget.disabled, "enabled with nothing to forget");
  assert(
    off.els.forget.text.includes("ansible"),
    "with no committed value it must say what decides instead: " + off.els.forget.text
  );
});

// Pressing it used to close the panel, so the thing you restored to was only visible after
// reopening. It applies in place now.
check("restoring repopulates the list in place and disables the button", () => {
  const page = render([file("mine.ini")], ["mine.ini"], "local", "ansible.cfg", [], null,
    true, ["shared.ini"]);
  const p = runPanel(scriptOf(page), page);
  p.els.forget.fire("click");
  assert(
    (p.sent[p.sent.length - 1] || {}).type === "forget",
    "the button did not ask the host to forget"
  );
  p.onMessage({ type: "restored", paths: ["shared.ini"] });
  assert(allText(p.els.sel).includes("shared.ini"), "the list did not repopulate");
  assert(p.els.forget.disabled, "the button stayed live with nothing left to forget");
});

// The store rule itself, not just the panel. This is the bug the user hit: choosing "just
// for me" appeared to do nothing, because an empty local pick fell back to the committed
// setting instead of overriding it.
check("an empty local pick overrides the committed setting; an absent one does not", () => {
  const r = sandbox.resolveInventory;
  assert(r, "resolveInventory is not reachable");
  assert(seq(r(undefined, ["shared.ini"])) === seq(["shared.ini"]), "never chose -> shared");
  assert(seq(r([], ["shared.ini"])) === seq([]), "chose none -> none, not the shared value");
  assert(seq(r(["mine.ini"], ["shared.ini"])) === seq(["mine.ini"]), "chose one -> that one");
});

// The restore button's hover text depends on state, so it is set on every render — and the
// button, unlike a row, survives a render. Re-binding there leaks a listener per repaint.
check("hover text updates on repaint without stacking listeners", () => {
  const page = render([file("a.ini")], ["a.ini"], "local", "ansible.cfg", [], null, true, []);
  const p = runPanel(scriptOf(page), page);
  const before = (p.els.forget.handlers.mouseenter || []).length;
  assert(before === 1, "expected one mouseenter handler, got " + before);

  // Force several repaints by toggling a row off and on.
  const cross = walk(p.els.sel).filter((e) => e.tagName === "button" && e.text === "\u2715")[0];
  cross.fire("click");
  walk(p.els.avail).filter((e) => e.className === "add")[0].fire("click");
  const after = (p.els.forget.handlers.mouseenter || []).length;
  assert(after === 1, "listeners stacked across repaints: " + after);

  // And the DISPLAYED text tracks the current state, not the one captured at first bind.
  const tipEl = p.els.body.children.find((c) => (c.className || "").includes("tip"));
  p.els.forget.fire("mouseenter", { clientX: 10, clientY: 10 });
  assert(
    tipEl.text.includes("Forgets the inventory"),
    "expected the live wording, got: " + tipEl.text
  );
  p.onMessage({ type: "restored", paths: [] });
  p.els.forget.fire("mouseenter", { clientX: 10, clientY: 10 });
  assert(
    tipEl.text.includes("Nothing to forget"),
    "the hover text was frozen at the first render: " + tipEl.text
  );
});

process.exit(failed ? 1 : 0);
