#!/usr/bin/env node
// End-to-end smoke test: drive ansible-lsp over raw stdio, no VS Code involved.
// Puts a cursor on an include_tasks value, asks for the definition, and prints where it
// resolved to. The project is built in a temp dir (see fixture.js) so this runs on a fresh
// checkout; pass a project root as the first argument to drive a real repo instead.

const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

const { build, toUri, serverBin, CASES } = require("./fixture");

const BIN = serverBin();
const { root: REPO, generated } = build();

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
  console.log(`project: ${REPO}${generated ? " (generated)" : ""}\n`);
  await request("initialize", {
    processId: process.pid,
    rootUri: toUri(REPO),
    workspaceFolders: [{ uri: toUri(REPO), name: "ansible" }],
    capabilities: {},
  });
  notify("initialized", {});

  let pass = 0;
  let skipped = 0;
  for (const [rel, needle, expect] of CASES) {
    const abs = path.join(REPO, rel);
    if (!fs.existsSync(abs)) {
      // Only reachable when driving a real repo that lacks the file; the generated tree
      // always has all four, so a skip there would be a fixture bug.
      console.log(`SKIP  ${rel} (not present)`);
      skipped++;
      continue;
    }
    const text = fs.readFileSync(abs, "utf8");
    const uri = toUri(abs);

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
        got ? got.replace(`${toUri(REPO)}/`, "") : "no definition returned"
      }`
    );
  }

  console.log(`\n${pass}/${CASES.length - skipped} resolved`);

  // documentLink over the demo file: exactly what the client paints teal.
  const demo = path.join(__dirname, "..", "demo", "tasks", "main.yml");
  if (fs.existsSync(demo)) {
    const text = fs.readFileSync(demo, "utf8");
    const uri = toUri(demo);
    notify("textDocument/didOpen", {
      textDocument: { uri, languageId: "ansible", version: 1, text },
    });
    const res = await request("ansible/references", { uri });
    const refs = res.result || [];
    const lines = text.split("\n");
    console.log(`\ndemo/tasks/main.yml — ${refs.length} references coloured:`);
    for (const l of refs) {
      const value = lines[l.range.start.line].slice(
        l.range.start.character,
        l.range.end.character
      );
      const many = l.targets > 1 ? `  <- ${l.targets} targets` : "";
      console.log(`  line ${String(l.range.start.line + 1).padStart(2)}  ${value}${many}`);
    }

    // The multi-target case must come back from goto-definition, not documentLink.
    const multi = refs.find((r) => r.targets > 1);
    if (multi) {
      const def = await request("textDocument/definition", {
        textDocument: { uri },
        position: { line: multi.range.start.line, character: multi.range.start.character + 1 },
      });
      const n = (def.result || []).length;
      console.log(`\n  goto-definition on line ${multi.range.start.line + 1} returned ${n} location(s):`);
      for (const l of def.result || []) console.log(`    ${l.uri.split("/demo/")[1] || l.uri}`);
    }
  }

  // Diagnostics arrive as notifications after didOpen.
  await new Promise((r) => setTimeout(r, 200));
  const demoDiags = diagnostics.get(toUri(demo)) || [];
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
  // site.yml is never opened above — it carries a deliberately missing include so the scan
  // has something to find.
  process.stdout.write("\nwaiting for workspace scan");
  for (let i = 0; i < 40 && !diagnostics.has(`${toUri(REPO)}/site.yml`); i++) {
    process.stdout.write(".");
    await new Promise((r) => setTimeout(r, 250));
  }
  const scanned = [...diagnostics.entries()].filter(([u, d]) => d.length && !u.includes("/demo/"));
  console.log(`\n\nrepo-wide scan flagged ${scanned.length} file(s) never opened:`);
  for (const [uri, d] of scanned) {
    console.log(`  ${uri.replace(`${toUri(REPO)}/`, "")}:${d[0].range.start.line + 1}`);
    console.log(`      ${d[0].message.split("\n")[0]}`);
  }

  await request("shutdown", null);
  notify("exit", null);
  srv.kill();
  const wanted = CASES.length - skipped;
  process.exit(pass === wanted && (generated ? scanned.length > 0 : true) ? 0 : 1);
})();
