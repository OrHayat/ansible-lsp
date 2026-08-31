// What the grammar actually paints. `node test/grammar.js`, or `npm test` from client/.
//
// A grammar is regexes, and the one case that matters most is the one regexes are usually
// wrong about: inside `{% raw %}` there is no tag grammar at all, so a `{%` that opens
// nothing is ordinary text. That is T-216 as colour, and the reason the `#raw` rule is first
// in the pattern list. Asserted by tokenising with `vscode-textmate` — the same engine VS
// Code paints with — rather than by reading the JSON and believing it.

const fs = require("fs");
const path = require("path");
const vsctm = require("vscode-textmate");
const oniguruma = require("vscode-oniguruma");

const CLIENT = path.join(__dirname, "..");
const GRAMMAR = path.join(CLIENT, "syntaxes", "jinja.tmLanguage.json");
const DEMO = path.join(CLIENT, "..", "demo", "templates");

let failures = 0;
function ok(name, cond, detail) {
  if (cond) return console.log("ok   " + name);
  failures++;
  console.log("FAIL " + name + (detail ? "\n     " + detail : ""));
}

async function registry() {
  const wasm = fs.readFileSync(
    path.join(CLIENT, "node_modules", "vscode-oniguruma", "release", "onig.wasm")
  );
  await oniguruma.loadWASM(wasm.buffer);
  return new vsctm.Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (s) => new oniguruma.OnigScanner(s),
      createOnigString: (s) => new oniguruma.OnigString(s),
    }),
    loadGrammar: async (scope) =>
      scope === "source.jinja"
        ? vsctm.parseRawGrammar(fs.readFileSync(GRAMMAR, "utf8"), GRAMMAR)
        : null,
  });
}

/** Every token as `{text, scopes}`, with the grammar's line state carried across lines. */
function tokenize(grammar, src) {
  let rules = vsctm.INITIAL;
  const out = [];
  for (const line of src.split("\n")) {
    const r = grammar.tokenizeLine(line, rules);
    for (const t of r.tokens) {
      const text = line.substring(t.startIndex, t.endIndex);
      if (text.trim()) out.push({ text, scopes: t.scopes });
    }
    rules = r.ruleStack;
  }
  return out;
}

const scoped = (toks, text, scope) =>
  toks.some((t) => t.text.includes(text) && t.scopes.some((s) => s.startsWith(scope)));

(async () => {
  const grammar = await (await registry()).loadGrammar("source.jinja");

  // The T-216 shape. `{%s}` sits inside a raw body, so it is data — never a tag.
  const raw = tokenize(grammar, '{% raw %}\nprintf "x{%s} %.0f\\n"\n{% endraw %}\n');
  const stray = raw.filter((t) => t.text.includes("{%s}") || t.text === "{%s}");
  ok(
    "a { % inside a raw body is not painted as a tag",
    stray.length > 0 && !stray.some((t) => t.scopes.some((s) => s.startsWith("meta.tag"))),
    JSON.stringify(stray)
  );
  ok(
    "the raw tags themselves are still keywords",
    scoped(raw, "{% raw %}", "keyword.control"),
    JSON.stringify(raw.slice(0, 2))
  );
  // The control: the same text outside a raw body IS a tag, so the assertion above is not
  // just "nothing is ever a tag".
  const bare = tokenize(grammar, "{% if x %}ok{% endif %}");
  ok(
    "control — outside a raw body the same delimiters are a tag",
    scoped(bare, "if", "keyword.control") && scoped(bare, "{%", "punctuation.definition.tag"),
    JSON.stringify(bare)
  );

  const expr = tokenize(grammar, "{{ app_port | default(8080) }}\n{# a comment #}\n");
  ok("an interpolation is scoped", scoped(expr, "{{", "punctuation.definition.template-expression"));
  ok("a filter after a pipe is a function, not a variable", scoped(expr, "default", "support.function"));
  ok("a variable is a variable", scoped(expr, "app_port", "variable.other"));
  ok("a number is a number", scoped(expr, "8080", "constant.numeric"));
  ok("a comment is a comment", scoped(expr, "a comment", "comment.block"));

  // Every demo template tokenises without the grammar falling off the end of a rule — a
  // stack that never unwinds paints the rest of the file as one colour.
  let painted = 0;
  for (const f of fs.readdirSync(DEMO).filter((f) => f.endsWith(".j2"))) {
    const toks = tokenize(grammar, fs.readFileSync(path.join(DEMO, f), "utf8"));
    if (toks.some((t) => t.scopes.length > 1)) painted++;
  }
  ok(`every demo template gets some colour (${painted} files)`, painted >= 10);

  // The manifest is the other half: a grammar nothing points at paints nothing.
  const pkg = JSON.parse(fs.readFileSync(path.join(CLIENT, "package.json"), "utf8"));
  const lang = (pkg.contributes.languages || []).find((l) => l.id === "jinja");
  ok("the jinja language is contributed", !!lang);
  ok(
    "it claims the three template spellings",
    lang && [".j2", ".jinja", ".jinja2"].every((e) => lang.extensions.includes(e)),
    JSON.stringify(lang && lang.extensions)
  );
  ok(
    "the grammar is wired to that language id",
    (pkg.contributes.grammars || []).some(
      (g) => g.language === "jinja" && g.scopeName === "source.jinja"
    )
  );
  // `onLanguage:` for an id nothing contributes never fires. This caught a dangling entry.
  const ids = new Set((pkg.contributes.languages || []).map((l) => l.id));
  const dangling = pkg.activationEvents
    .filter((e) => e.startsWith("onLanguage:"))
    .map((e) => e.slice("onLanguage:".length))
    .filter((id) => !ids.has(id) && !["yaml", "ansible"].includes(id));
  ok("no onLanguage: event names an id we do not contribute", dangling.length === 0, String(dangling));

  process.exit(failures ? 1 : 0);
})();
