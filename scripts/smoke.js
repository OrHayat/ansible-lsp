#!/usr/bin/env node
// End-to-end smoke test: drive ansible-lsp over raw stdio, no VS Code involved.
// Opens a real file from ~/matrix/ansible, asks for the definition under a cursor
// placed on an include_tasks value, and prints where it resolved to.

const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

const BIN = path.join(__dirname, "..", "target", "release", "ansible-lsp");
const REPO = path.join(process.env.HOME, "matrix", "ansible");

// [file, the include value to click on, what we expect to land in]
const CASES = [
  [
    "roles/sync-state/tasks/nfs_access_point/reconcile.yml",
    "_converge_one_ap.yml",
    "nfs_access_point/_converge_one_ap.yml",
  ],
  [
    "roles/daos-snapshot/tasks/query/timestamp.yml",
    "query/exists.yml",
    "daos-snapshot/tasks/query/exists.yml",
  ],
  [
    "roles/ad/tasks/join.yml",
    "../../playbooks/tasks/select-available-node.yml",
    "playbooks/tasks/select-available-node.yml",
  ],
  [
    "roles/nautobot-docker/tasks/sanity-tests/main.yml",
    "sanity-tests/database-tests.yml",
    "sanity-tests/database-tests.yml",
  ],
];

const srv = spawn(BIN, [], { stdio: ["pipe", "pipe", "inherit"] });

let seq = 0;
const pending = new Map();
const diagnostics = new Map();
let buf = Buffer.alloc(0);

srv.stdout.on("data", (chunk) => {
  buf = Buffer.concat([buf, chunk]);
  for (;;) {
    const headerEnd = buf.indexOf("\r\n\r\n");
    if (headerEnd === -1) return;
    const header = buf.subarray(0, headerEnd).toString();
    const len = parseInt(/Content-Length: (\d+)/i.exec(header)[1], 10);
    const start = headerEnd + 4;
    if (buf.length < start + len) return;
    const msg = JSON.parse(buf.subarray(start, start + len).toString());
    buf = buf.subarray(start + len);
    if (msg.method === "textDocument/publishDiagnostics") {
      diagnostics.set(msg.params.uri, msg.params.diagnostics);
    }
    if (msg.id !== undefined && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  }
});

function send(obj) {
  const body = JSON.stringify(obj);
  srv.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

function request(method, params) {
  const id = ++seq;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    send({ jsonrpc: "2.0", id, method, params });
  });
}

function notify(method, params) {
  send({ jsonrpc: "2.0", method, params });
}

// Locate `needle` in the text and return a 0-based LSP position pointing into it.
// Counts UTF-16 code units, exactly as LSP requires — so a line containing an em
// dash or emoji before the target still produces the right column.
function positionOf(text, needle) {
  const byteIdx = text.indexOf(needle);
  if (byteIdx === -1) throw new Error(`not found in file: ${needle}`);
  const before = text.slice(0, byteIdx);
  const line = (before.match(/\n/g) || []).length;
  const lineStart = before.lastIndexOf("\n") + 1;
  const character = text.slice(lineStart, byteIdx).length + 2; // a couple of chars in
  return { line, character };
}

(async () => {
  await request("initialize", {
    processId: process.pid,
    rootUri: `file://${REPO}`,
    workspaceFolders: [{ uri: `file://${REPO}`, name: "ansible" }],
    capabilities: {},
  });
  notify("initialized", {});

  let pass = 0;
  for (const [rel, needle, expect] of CASES) {
    const abs = path.join(REPO, rel);
    if (!fs.existsSync(abs)) {
      console.log(`SKIP  ${rel} (not present)`);
      continue;
    }
    const text = fs.readFileSync(abs, "utf8");
    const uri = `file://${abs}`;

    notify("textDocument/didOpen", {
      textDocument: { uri, languageId: "ansible", version: 1, text },
    });

    const position = positionOf(text, needle);
    const res = await request("textDocument/definition", {
      textDocument: { uri },
      position,
    });

    const got = res.result?.[0]?.uri;
    const ok = got && got.includes(expect);
    if (ok) pass++;
    console.log(
      `${ok ? "PASS" : "FAIL"}  ${needle}\n      -> ${
        got ? got.replace(`file://${REPO}/`, "") : "no definition returned"
      }`
    );
  }

  console.log(`\n${pass}/${CASES.length} resolved`);

  // documentLink over the demo file: exactly what the client paints teal.
  const demo = path.join(__dirname, "..", "demo", "tasks", "main.yml");
  if (fs.existsSync(demo)) {
    const text = fs.readFileSync(demo, "utf8");
    const uri = `file://${demo}`;
    notify("textDocument/didOpen", {
      textDocument: { uri, languageId: "ansible", version: 1, text },
    });
    const res = await request("textDocument/documentLink", {
      textDocument: { uri },
    });
    const links = res.result || [];
    const lines = text.split("\n");
    console.log(`\ndemo/tasks/main.yml — ${links.length} references will be coloured:`);
    for (const l of links) {
      const line = lines[l.range.start.line];
      const value = line.slice(l.range.start.character, l.range.end.character);
      console.log(`  line ${String(l.range.start.line + 1).padStart(2)}  ${value}`);
    }
  }

  // Diagnostics arrive as notifications after didOpen.
  await new Promise((r) => setTimeout(r, 200));
  const demoDiags = diagnostics.get(`file://${demo}`) || [];
  const demoText = fs.readFileSync(demo, "utf8").split("\n");
  console.log(`\ndemo/tasks/main.yml — ${demoDiags.length} warnings:`);
  for (const d of demoDiags) {
    const value = demoText[d.range.start.line].slice(
      d.range.start.character,
      d.range.end.character
    );
    console.log(`  line ${d.range.start.line + 1}  ${value}`);
    console.log(`      ${d.message.split("\n")[0]}`);
  }

  // Repo-wide scan: diagnostics for files we never opened.
  process.stdout.write("\nwaiting for workspace scan");
  for (let i = 0; i < 40 && !diagnostics.has(`file://${REPO}/site.yml`); i++) {
    process.stdout.write(".");
    await new Promise((r) => setTimeout(r, 250));
  }
  const scanned = [...diagnostics.entries()].filter(([u, d]) => d.length && !u.includes("/demo/"));
  console.log(`\n\nrepo-wide scan flagged ${scanned.length} file(s) never opened:`);
  for (const [uri, d] of scanned) {
    console.log(`  ${uri.replace(`file://${REPO}/`, "")}:${d[0].range.start.line + 1}`);
    console.log(`      ${d[0].message.split("\n")[0]}`);
  }

  await request("shutdown", null);
  notify("exit", null);
  srv.kill();
  process.exit(pass === CASES.length ? 0 : 1);
})();
