// What VS Code is told to send us. `node test/selector.js`, or `npm test` from client/.
//
// The server was verified end to end by driving the real binary over LSP stdio, but that
// probe sent the `didOpen` itself. It proves the server answers when told about a `.j2`; it
// cannot prove the editor will tell it. That is this selector's job, and a selector that
// matches nothing fails silently — no error, no log, just a file the server never hears
// about. Exactly the shape `webview.js` exists for, one layer out.
//
// Matched with `minimatch`, which is what `vscode-languageclient` resolves a
// `DocumentFilter.pattern` through.

const fs = require("fs");
const path = require("path");
const Module = require("module");
// minimatch 3 exports the function itself; 5+ exports it as a named field.
const mm = require("minimatch");
const minimatch = typeof mm === "function" ? mm : mm.minimatch;

const SRC = path.join(__dirname, "..", "src", "extension.js");
const DEMO = path.join(__dirname, "..", "..", "demo");

// The real `activate`, with the two things it touches stubbed and the LanguageClient
// replaced by a recorder — so this reads the selector the client actually builds rather
// than a copy of it, which is the mistake this repo has already made twice.
let built = null;
const stubs = {
  vscode: {
    window: {
      createTextEditorDecorationType: () => ({}),
      createStatusBarItem: () => ({ show() {}, hide() {}, dispose() {} }),
      showErrorMessage: (m) => {
        throw new Error("activate bailed: " + m);
      },
      onDidChangeActiveTextEditor: () => ({ dispose() {} }),
      onDidChangeVisibleTextEditors: () => ({ dispose() {} }),
      visibleTextEditors: [],
      createOutputChannel: () => ({ appendLine() {}, show() {}, dispose() {} }),
      showInformationMessage: () => Promise.resolve(undefined),
      showWarningMessage: () => Promise.resolve(undefined),
      registerWebviewViewProvider: () => ({ dispose() {} }),
      activeTextEditor: undefined,
    },
    workspace: {
      getConfiguration: () => ({ get: (_k, d) => d }),
      onDidChangeTextDocument: () => ({ dispose() {} }),
      onDidChangeConfiguration: () => ({ dispose() {} }),
      onDidSaveTextDocument: () => ({ dispose() {} }),
      workspaceFolders: [],
      onDidChangeWorkspaceFolders: () => ({ dispose() {} }),
      createFileSystemWatcher: () => ({
        onDidChange: () => ({ dispose() {} }),
        onDidCreate: () => ({ dispose() {} }),
        onDidDelete: () => ({ dispose() {} }),
        dispose() {},
      }),
      onDidCloseTextDocument: () => ({ dispose() {} }),
      onDidOpenTextDocument: () => ({ dispose() {} }),
    },
    StatusBarAlignment: { Left: 1 },
    ViewColumn: { Active: -1 },
    ConfigurationTarget: { Workspace: 2 },
    commands: { registerCommand: () => ({ dispose() {} }) },
    Range: class {},
  },
  "vscode-languageclient/node": {
    LanguageClient: class {
      constructor(_id, _name, _server, options) {
        built = options;
      }
      start() {
        return Promise.resolve();
      }
      stop() {}
      onNotification() {}
      isRunning() {
        return false;
      }
    },
    TransportKind: { stdio: "stdio" },
  },
};
const load = Module._load;
Module._load = (req, parent, isMain) =>
  req in stubs ? stubs[req] : load(req, parent, isMain);

const { activate } = require(SRC);

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

// A context whose serverPath resolves to something that exists, so `activate` gets past its
// own existence check and reaches the client construction.
const context = {
  subscriptions: [],
  extensionPath: path.join(__dirname, ".."),
  asAbsolutePath: (p) => path.join(__dirname, "..", p),
  workspaceState: { get: (_k, d) => d, update: () => Promise.resolve() },
  globalState: { get: (_k, d) => d, update: () => Promise.resolve() },
};

check("activate builds a client", () => {
  activate(context);
  assert(built, "LanguageClient was never constructed — activate bailed before the selector");
  assert(Array.isArray(built.documentSelector), "no documentSelector");
});

// Does a path match any glob filter in the selector? This is the question VS Code asks.
function matches(rel) {
  const posix = rel.split(path.sep).join("/");
  return (built.documentSelector || []).some(
    (f) => f.pattern && minimatch(posix, f.pattern)
  );
}

check("every template in demo/ is sent to the server", () => {
  const found = [];
  (function walk(dir) {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) walk(p);
      else if (p.endsWith(".j2")) found.push(path.relative(DEMO, p));
    }
  })(DEMO);
  assert(found.length > 5, "the demo walk found no templates: " + found.length);
  const missed = found.filter((f) => !matches(f));
  assert(missed.length === 0, "not matched by any filter: " + missed.join(", "));
});

check("the other two template spellings match too", () => {
  for (const f of ["a.jinja", "x/y/b.jinja2", "deep/nested/c.j2"]) {
    assert(matches(f), f + " is not matched");
  }
});

// The control. A selector of `**/*` would pass everything above and be wrong: YAML must
// reach the server through its *language* filter, not the glob, or a `.yml` in a workspace
// with no yaml extension installed would be analysed as a template.
check("the glob does not swallow non-templates", () => {
  for (const f of ["playbook.yml", "roles/r/tasks/main.yml", "ansible.cfg", "README.md"]) {
    assert(!matches(f), f + " is matched by the template glob and must not be");
  }
});

check("yaml and ansible still arrive by language id", () => {
  const langs = (built.documentSelector || []).map((f) => f.language).filter(Boolean);
  assert(langs.includes("yaml"), "yaml filter gone");
  assert(langs.includes("ansible"), "ansible filter gone");
});

process.exit(failed ? 1 : 0);
