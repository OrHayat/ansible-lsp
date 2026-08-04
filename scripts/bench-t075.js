#!/usr/bin/env node
// T-075 bench: is the server responsive WHILE the workspace scan runs?
// Measures, from the `initialized` notification:
//   1. first documentLink response   (was: blocked behind the whole scan)
//   2. first hover response
//   3. the scan-complete log line    (total scan wall clock, T-074 format)
// Usage: node bench-t075.js <path-to-ansible-lsp-binary> <workspace-root>

const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

const BIN = process.argv[2];
const ROOT = path.resolve(process.argv[3]);
const FILE = path.join(ROOT, "tasks", "main.yml");

const srv = spawn(BIN, [], { stdio: ["pipe", "pipe", "inherit"] });
let seq = 0;
const pending = new Map();
let buf = Buffer.alloc(0);
let t0 = null;
let scanLine = null;

const done = () => {
  if (scanLine === null) return;
  srv.kill();
  process.exit(0);
};

srv.stdout.on("data", (chunk) => {
  buf = Buffer.concat([buf, chunk]);
  for (;;) {
    const he = buf.indexOf("\r\n\r\n");
    if (he === -1) return;
    const len = parseInt(/Content-Length: (\d+)/i.exec(buf.subarray(0, he).toString())[1], 10);
    const start = he + 4;
    if (buf.length < start + len) return;
    const msg = JSON.parse(buf.subarray(start, start + len).toString());
    buf = buf.subarray(start + len);
    if (msg.id !== undefined && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    } else if (msg.method === "window/logMessage" && /ansible-lsp detect:/.test(msg.params.message)) {
      console.log(`detect finished    ${ms()} ms   (${msg.params.message.trim()})`);
    } else if (msg.method === "window/logMessage" && /ansible-lsp scan:/.test(msg.params.message)) {
      scanLine = ms();
      console.log(`scan finished      ${scanLine} ms   (${msg.params.message.trim()})`);
      done();
    }
  }
});

const send = (o) => {
  const b = JSON.stringify(o);
  srv.stdin.write(`Content-Length: ${Buffer.byteLength(b)}\r\n\r\n${b}`);
};
const request = (method, params) => {
  const id = ++seq;
  return new Promise((r) => {
    pending.set(id, r);
    send({ jsonrpc: "2.0", id, method, params });
  });
};
const notify = (method, params) => send({ jsonrpc: "2.0", method, params });
const ms = () => (Number(process.hrtime.bigint() - t0) / 1e6).toFixed(1);

(async () => {
  await request("initialize", { processId: process.pid, rootUri: `file://${ROOT}`, capabilities: {} });
  t0 = process.hrtime.bigint();
  notify("initialized", {});

  const text = fs.readFileSync(FILE, "utf8");
  const uri = `file://${FILE}`;
  notify("textDocument/didOpen", { textDocument: { uri, languageId: "ansible", version: 1, text } });

  const link = await request("textDocument/documentLink", { textDocument: { uri } });
  console.log(`documentLink       ${ms()} ms   (${(link.result || []).length} links)`);
  const hov = await request("textDocument/hover", {
    textDocument: { uri },
    position: { line: 0, character: 4 },
  });
  console.log(`hover              ${ms()} ms   (${hov.result ? "content" : "empty"})`);
  setTimeout(() => { console.log("scan line never arrived (30s)"); srv.kill(); process.exit(1); }, 30000);
})();
