#!/usr/bin/env node
// T-012 over the wire: the server registers a file watcher with a client that supports it,
// and every kind of change on disk — create, delete, a role renamed, a role directory
// deleted, `ansible.cfg` edited — reaches the diagnostics of a file that did not change.
// Drives the release binary over raw stdio, as `smoke.js` does, acting as a client that
// supports dynamic registration. Exits non-zero on the first broken expectation.

const { spawn } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const { toUri, serverBin } = require("./fixture");

const ROOT = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "ansible-lsp-watch-")));
const write = (rel, text) => {
  fs.mkdirSync(path.dirname(path.join(ROOT, rel)), { recursive: true });
  fs.writeFileSync(path.join(ROOT, rel), text);
};
write("ansible.cfg", "[defaults]\nroles_path = ./roles\n");
write("site.yml", "- hosts: all\n  roles: [web]\n  tasks:\n    - include_tasks: missing.yml\n    - include_tasks: present.yml\n");
write("db.yml", "- hosts: all\n  roles: [db]\n");
write("present.yml", "- debug: {msg: here}\n");
write("roles/web/tasks/main.yml", "- debug: {msg: web}\n");
write("custom/db/tasks/main.yml", "- debug: {msg: db}\n");

const srv = spawn(serverBin(), [], { stdio: ["pipe", "pipe", "inherit"] });
let seq = 0;
const pending = new Map();
const diagnostics = new Map();
const publishes = new Map();
let registered = null;
let scans = 0;
let buf = Buffer.alloc(0);

function send(obj) {
  const body = JSON.stringify(obj);
  srv.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

srv.stdout.on("data", (chunk) => {
  buf = Buffer.concat([buf, chunk]);
  for (;;) {
    const headerEnd = buf.indexOf("\r\n\r\n");
    if (headerEnd === -1) return;
    const len = parseInt(/Content-Length: (\d+)/i.exec(buf.subarray(0, headerEnd).toString())[1], 10);
    const start = headerEnd + 4;
    if (buf.length < start + len) return;
    const msg = JSON.parse(buf.subarray(start, start + len).toString());
    buf = buf.subarray(start + len);
    if (msg.method === "client/registerCapability") {
      registered = msg.params.registrations;
      send({ jsonrpc: "2.0", id: msg.id, result: null });
    } else if (msg.id !== undefined && msg.method) {
      send({ jsonrpc: "2.0", id: msg.id, result: null });
    } else if (msg.method === "textDocument/publishDiagnostics") {
      diagnostics.set(msg.params.uri, msg.params.diagnostics);
      publishes.set(msg.params.uri, (publishes.get(msg.params.uri) || 0) + 1);
    } else if (msg.method === "window/logMessage" && msg.params.message.startsWith("ansible-lsp scan:")) {
      scans += 1;
    } else if (msg.id !== undefined && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  }
});

function request(method, params) {
  const id = ++seq;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    send({ jsonrpc: "2.0", id, method, params });
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const messages = (rel) => (diagnostics.get(toUri(path.join(ROOT, rel))) || []).map((d) => d.message);
const mentions = (rel, needle) => messages(rel).some((m) => m.includes(needle));
const event = (changes) =>
  send({
    jsonrpc: "2.0",
    method: "workspace/didChangeWatchedFiles",
    params: { changes: changes.map(([rel, type]) => ({ uri: toUri(path.join(ROOT, rel)), type })) },
  });
const [CREATED, CHANGED, DELETED] = [1, 2, 3];

async function until(what, ok) {
  for (let i = 0; i < 100; i++) {
    if (ok()) return console.log(`ok    ${what}`);
    await sleep(50);
  }
  console.log(`FAIL  ${what}`);
  srv.kill();
  process.exit(1);
}

(async () => {
  await request("initialize", {
    processId: process.pid,
    rootUri: toUri(ROOT),
    workspaceFolders: [{ uri: toUri(ROOT), name: "watch" }],
    capabilities: { workspace: { didChangeWatchedFiles: { dynamicRegistration: true } } },
  });
  send({ jsonrpc: "2.0", method: "initialized", params: {} });

  await until("the watcher is registered for every file", () =>
    registered?.some((r) => r.method === "workspace/didChangeWatchedFiles" && r.registerOptions.watchers[0].globPattern === "**/*"),
  );
  await until("the first scan warns on missing.yml and on role db", () => scans >= 1 && mentions("site.yml", "missing.yml") && messages("db.yml").length > 0);

  write("missing.yml", "- debug: {msg: now}\n");
  event([["missing.yml", CREATED]]);
  await until("creating missing.yml clears its warning", () => !mentions("site.yml", "missing.yml"));

  fs.rmSync(path.join(ROOT, "present.yml"));
  event([["present.yml", DELETED]]);
  await until("deleting present.yml warns", () => mentions("site.yml", "present.yml"));

  // A rename arrives as a delete and a create; one publish after the burst, never the
  // intermediate state.
  const before = publishes.get(toUri(path.join(ROOT, "site.yml"))) || 0;
  fs.renameSync(path.join(ROOT, "roles/web"), path.join(ROOT, "roles/web2"));
  event([["roles/web/tasks/main.yml", DELETED]]);
  await sleep(100);
  event([["roles/web2/tasks/main.yml", CREATED]]);
  await until("renaming role web warns where it is used", () => messages("site.yml").some((m) => /\bweb\b/.test(m) && !m.includes("web2")));
  await sleep(700);
  const after = publishes.get(toUri(path.join(ROOT, "site.yml"))) || 0;
  if (after - before !== 1) {
    console.log(`FAIL  the rename published site.yml ${after - before} times, not once`);
    srv.kill();
    process.exit(1);
  }
  console.log("ok    the rename is one publish, no flicker");

  fs.renameSync(path.join(ROOT, "roles/web2"), path.join(ROOT, "roles/web"));
  event([["roles/web", CREATED], ["roles/web2", DELETED]]);
  await until("the role back under its name clears it", () => !messages("site.yml").some((m) => /\bweb\b/.test(m)));
  fs.rmSync(path.join(ROOT, "roles/web"), { recursive: true });
  event([["roles/web", DELETED]]);
  await until("deleting the role directory, reported as the directory alone, warns", () => messages("site.yml").some((m) => /\bweb\b/.test(m)));

  write("ansible.cfg", "[defaults]\nroles_path = ./roles:./custom\n");
  event([["ansible.cfg", CHANGED]]);
  await until("adding custom/ to roles_path in ansible.cfg resolves role db", () => messages("db.yml").length === 0);

  srv.kill();
  console.log(`\nall ok, ${scans} scans`);
  process.exit(0);
})();
