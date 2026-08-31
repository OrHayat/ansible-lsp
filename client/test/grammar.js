// What the grammar paints, and — mostly — what it deliberately does NOT. `node test/grammar.js`.
//
// The grammar used to colour tags. It was wrong to: a grammar hardcodes `{%`, while a
// template's real delimiters can come from a `#jinja2:` header or from the `template:` task
// that renders it. Measured in the editor on `demo/templates/overridden.conf.j2`: the server
// correctly read `<% include %>` as a tag AND correctly read the literal `{% notatag %}` as
// output, and the grammar painted that literal as a tag anyway. A semantic token overrides a
// grammar scope only where it provides one, so "this is data" is unsayable and the grammar's
// guess stands. The fix is for the grammar not to guess.
//
// So these assertions are mostly negative, and that is the point. Tokenised with
// `vscode-textmate`, the engine VS Code paints with.

const fs = require("fs");
const path = require("path");
const vsctm = require("vscode-textmate");
const oniguruma = require("vscode-oniguruma");

const CLIENT = path.join(__dirname, "..");
const GRAMMAR = path.join(CLIENT, "syntaxes", "jinja.tmLanguage.json");

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

/** Anything beyond the root scope — i.e. the grammar made a claim about this text. */
const claims = (toks, text) =>
  toks.filter((t) => t.text.includes(text) && t.scopes.some((s) => s !== "source.jinja"));

(async () => {
  const grammar = await (await registry()).loadGrammar("source.jinja");

  // The core of it: the grammar makes no claim about a tag, in either direction. Under an
  // overridden delimiter this text is literal output, and the grammar cannot know which.
  for (const src of [
    "{% if x %}ok{% endif %}",
    "{% notatag %}",
    "{# a comment #}",
    "{% raw %}printf \"x{%s}\"{% endraw %}",
  ]) {
    ok(`no claim about ${JSON.stringify(src.slice(0, 24))}`, claims(tokenize(grammar, src), src.slice(0, 6)).length === 0,
       JSON.stringify(tokenize(grammar, src)));
  }

  // The one thing it does keep: `{{ }}` as neutral punctuation, so a template still reads as
  // structured before the server answers. It names no keyword and no variable, so the tokens
  // refine it rather than contradict it.
  const expr = tokenize(grammar, "{{ app_port | default(8080) }}");
  ok("the {{ }} delimiters are punctuation", claims(expr, "{{").length === 1, JSON.stringify(expr));
  ok(
    "and nothing inside them is claimed — that is the server's answer",
    ["app_port", "default", "8080"].every((w) => claims(expr, w).length === 0),
    JSON.stringify(expr)
  );

  // The manifest half: a grammar nothing points at paints nothing, and an `onLanguage:` for an
  // id we do not contribute never fires.
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
  const ids = new Set((pkg.contributes.languages || []).map((l) => l.id));
  const dangling = pkg.activationEvents
    .filter((e) => e.startsWith("onLanguage:"))
    .map((e) => e.slice("onLanguage:".length))
    .filter((id) => !ids.has(id) && !["yaml", "ansible"].includes(id));
  ok("no onLanguage: event names an id we do not contribute", dangling.length === 0, String(dangling));

  process.exit(failures ? 1 : 0);
})();
