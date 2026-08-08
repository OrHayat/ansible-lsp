// A throwaway Ansible project for the stdio harnesses to drive the server against.
//
// T-077: `smoke.js` and `timing.js` both opened `~/app/ansible` and named specific files
// inside it, so on any other machine they skipped every case or crashed outright. The tree
// below is written from string literals at run time, so the harnesses state exactly what
// they depend on and work on a fresh checkout.
//
// Pass a project root as the first CLI argument to run against a real repo instead.

const fs = require("fs");
const os = require("os");
const path = require("path");

function write(root, rel, body) {
  const p = path.join(root, rel);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, body);
  return p;
}

// Each case pins a different rule in the include search order.
const FILES = {
  "ansible.cfg": "[defaults]\nroles_path = ./roles\n",

  // A sibling include inside a tasks *subdirectory*: resolves next to the including file.
  "roles/sync-state/tasks/main.yml": "- include_tasks: http_access_point/reconcile.yml\n",
  "roles/sync-state/tasks/http_access_point/reconcile.yml":
    "- name: converge one access point\n  include_tasks: _converge_one_ap.yml\n",
  "roles/sync-state/tasks/http_access_point/_converge_one_ap.yml": "- debug: {msg: converged}\n",

  // Nested one level down, but written relative to the role's tasks/ dir — NOT to the
  // including file, which would give tasks/query/query/exists.yml.
  "roles/lustre-snapshot/tasks/query/timestamp.yml":
    "- name: read the timestamp\n  include_tasks: query/exists.yml\n",
  "roles/lustre-snapshot/tasks/query/exists.yml": "- debug: {msg: exists}\n",

  // Climbs out of the role entirely. Only resolves off the *role dir* as a base:
  // roles/ad/../../playbooks/... lands at the project root, while the tasks/ anchor
  // would land at roles/playbooks/... and miss.
  "roles/ad/tasks/join.yml":
    "- name: pick a node\n  include_tasks: ../../playbooks/tasks/select-available-node.yml\n",
  "playbooks/tasks/select-available-node.yml": "- debug: {msg: selected}\n",

  // Names its own subdirectory from inside it: anchors at tasks/, so the path is not
  // doubled into sanity-tests/sanity-tests/.
  "roles/dashboard-docker/tasks/sanity-tests/main.yml":
    "- name: database tests\n  include_tasks: sanity-tests/database-tests.yml\n",
  "roles/dashboard-docker/tasks/sanity-tests/database-tests.yml": "- debug: {msg: db ok}\n",

  // Never opened by the harness — it exists so the workspace scan has something to flag.
  "site.yml":
    "- hosts: all\n  tasks:\n    - name: this target does not exist\n      include_tasks: nowhere/missing.yml\n",
};

// [file, the include value to click on, what the definition must land in]
const CASES = [
  [
    "roles/sync-state/tasks/http_access_point/reconcile.yml",
    "_converge_one_ap.yml",
    "http_access_point/_converge_one_ap.yml",
  ],
  [
    "roles/lustre-snapshot/tasks/query/timestamp.yml",
    "query/exists.yml",
    "lustre-snapshot/tasks/query/exists.yml",
  ],
  [
    "roles/ad/tasks/join.yml",
    "../../playbooks/tasks/select-available-node.yml",
    "playbooks/tasks/select-available-node.yml",
  ],
  [
    "roles/dashboard-docker/tasks/sanity-tests/main.yml",
    "sanity-tests/database-tests.yml",
    "sanity-tests/database-tests.yml",
  ],
];

/// A playbook of `n` tasks, for timing a single big request. Sized against a real repo:
/// kubespray's largest YAML is ~1730 lines, its largest task file ~500.
function bigPlaybook(n) {
  let s = "- hosts: all\n  tasks:\n";
  for (let i = 0; i < n; i++) {
    // Resolves from playbooks/ — an unresolvable one would report zero links and time only
    // the miss path.
    s += `    - name: task ${i}\n      include_tasks: tasks/select-available-node.yml\n`;
  }
  return s;
}

// Returns the project root. `argv[2]`, when given, wins — so these still work against a
// real repo. `bigTasks` adds playbooks/big.yml for the timing harness.
function build({ bigTasks = 0 } = {}) {
  const given = process.argv[2];
  if (given) {
    if (!fs.existsSync(given)) {
      console.error(`no such project root: ${given}`);
      process.exit(2);
    }
    return { root: path.resolve(given), generated: false };
  }
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ansible-lsp-harness-"));
  for (const [rel, body] of Object.entries(FILES)) write(root, rel, body);
  if (bigTasks) write(root, "playbooks/big.yml", bigPlaybook(bigTasks));
  return { root, generated: true };
}

// `file://` needs a leading slash the Windows drive letter doesn't have, and forward
// slashes throughout — the harnesses run on both.
function toUri(p) {
  const abs = path.resolve(p).replace(/\\/g, "/");
  return abs.startsWith("/") ? `file://${abs}` : `file:///${abs}`;
}

// The built server. Windows needs the extension for `spawn` to find it.
function serverBin() {
  const base = path.join(__dirname, "..", "target", "release", "ansible-lsp");
  const p = process.platform === "win32" ? `${base}.exe` : base;
  if (!fs.existsSync(p)) {
    console.error(`no server binary at ${p}\nbuild it first: cargo build --release -p ansible-lsp`);
    process.exit(2);
  }
  return p;
}

module.exports = { build, toUri, serverBin, CASES, bigPlaybook };
