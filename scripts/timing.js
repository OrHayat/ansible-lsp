#!/usr/bin/env node
// How long does the server actually take? Spawn -> initialize -> requests.

const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

const BIN = path.join(__dirname, "..", "target", "release", "ansible-lsp");
const REPO = path.join(process.env.HOME, "matrix", "ansible");

const t0 = process.hrtime.bigint();
const srv = spawn(BIN, [], { stdio: ["pipe", "pipe", "inherit"] });
const tSpawn = Number(process.hrtime.bigint() - t0) / 1e6;

let seq = 0;
const pending = new Map();
let buf = Buffer.alloc(0);

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

const ms = (start) => (Number(process.hrtime.bigint() - start) / 1e6).toFixed(1);

(async () => {
  let t = process.hrtime.bigint();
  await request("initialize", { processId: process.pid, rootUri: `file://${REPO}`, capabilities: {} });
  const tInit = ms(t);
  notify("initialized", {});

  console.log(`process spawn      ${tSpawn.toFixed(1)} ms`);
  console.log(`initialize         ${tInit} ms`);

  // Biggest YAML file in the repo — worst case for a single request.
  let biggest = null;
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      if (e.isDirectory()) {
        if ([".git", "__pycache__", ".pytest_cache"].includes(e.name)) continue;
        walk(path.join(d, e.name));
      } else if (/\.ya?ml$/.test(e.name)) {
        const p = path.join(d, e.name);
        const s = fs.statSync(p).size;
        if (!biggest || s > biggest.size) biggest = { p, size: s };
      }
    }
  };
  walk(REPO);

  for (const [label, file] of [
    ["largest file", biggest.p],
    ["demo file", path.join(__dirname, "..", "demo", "tasks", "main.yml")],
  ]) {
    if (!fs.existsSync(file)) continue;
    const text = fs.readFileSync(file, "utf8");
    const uri = `file://${file}`;
    t = process.hrtime.bigint();
    notify("textDocument/didOpen", { textDocument: { uri, languageId: "ansible", version: 1, text } });
    const r = await request("textDocument/documentLink", { textDocument: { uri } });
    console.log(
      `documentLink       ${ms(t)} ms   (${label}, ${(text.length / 1024).toFixed(0)} KB, ${
        (r.result || []).length
      } links)`
    );
  }

  await request("shutdown", null);
  notify("exit", null);
  srv.kill();
})();
