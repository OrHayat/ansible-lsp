// Thin VS Code client: spawn the Rust server, speak LSP, and paint resolvable
// references so you can see what's clickable without hunting for it.

const path = require("path");
const fs = require("fs");
const vscode = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

// Teal + dotted underline, distinct from the theme's YAML value colours.
const linkDecoration = vscode.window.createTextEditorDecorationType({
  color: "#00BFA5",
  textDecoration: "underline dotted 1px",
});

function serverPath(context) {
  const configured = vscode.workspace
    .getConfiguration("ansibleLsp")
    .get("serverPath");
  return (
    configured ||
    context.asAbsolutePath(path.join("..", "target", "release", "ansible-lsp"))
  );
}

// Sent at startup and again on change. Normalised here so the server sees one shape
// rather than having to know VS Code's nesting.
function hintSettings() {
  const c = vscode.workspace.getConfiguration("ansibleLsp");
  return {
    inlayHints: {
      enabled: c.get("inlayHints.enabled", true),
      explanations: c.get("inlayHints.explanations", true),
    },
  };
}

function isYaml(doc) {
  return (
    doc &&
    (doc.languageId === "ansible" || doc.languageId === "yaml") &&
    doc.uri.scheme === "file"
  );
}

// Ask the server which references resolve, and colour exactly those.
//
// Deliberately not documentLink: the server only emits links for single-target
// references, because a link's target overrides the definition provider on
// Cmd+click and would swallow the extra candidates of a templated path.
async function paint(editor) {
  if (!editor || !isYaml(editor.document) || !client?.isRunning()) return;
  try {
    const refs = await client.sendRequest("ansible/references", {
      uri: editor.document.uri.toString(),
    });
    editor.setDecorations(
      linkDecoration,
      (refs || []).map((r) => ({
        range: new vscode.Range(
          r.range.start.line,
          r.range.start.character,
          r.range.end.line,
          r.range.end.character
        ),
        hoverMessage:
          r.targets > 1 ? `${r.targets} possible targets` : undefined,
      }))
    );
  } catch {
    // Server restarting or document closed; the next edit repaints.
  }
}

function activate(context) {
  const command = serverPath(context);

  if (!fs.existsSync(command)) {
    vscode.window.showErrorMessage(
      `ansible-lsp binary not found at ${command}. Run \`cargo build --release\`, or set ansibleLsp.serverPath.`
    );
    return;
  }

  client = new LanguageClient(
    "ansibleLsp",
    "Ansible LSP",
    { command, transport: TransportKind.stdio },
    {
      documentSelector: [
        { scheme: "file", language: "ansible" },
        { scheme: "file", language: "yaml" },
      ],
      initializationOptions: hintSettings(),
    }
  );

  const timers = new Map();
  const repaint = (editor) => {
    if (!editor) return;
    const key = editor.document.uri.toString();
    clearTimeout(timers.get(key));
    timers.set(key, setTimeout(() => paint(editor), 250));
  };

  context.subscriptions.push(
    linkDecoration,
    // Picks up a `cargo build --release` without reloading the whole window.
    vscode.commands.registerCommand("ansibleLsp.restart", async () => {
      await client.restart();
      vscode.window.visibleTextEditors.forEach(repaint);
      vscode.window.setStatusBarMessage("Ansible LSP restarted", 2000);
    }),
    // Settings take effect immediately; the server asks VS Code to re-request hints.
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (!e.affectsConfiguration("ansibleLsp.inlayHints")) return;
      client?.sendNotification("workspace/didChangeConfiguration", {
        settings: hintSettings(),
      });
    }),
    vscode.window.onDidChangeActiveTextEditor(repaint),
    vscode.window.onDidChangeVisibleTextEditors((es) => es.forEach(repaint)),
    vscode.workspace.onDidChangeTextDocument((e) => {
      for (const editor of vscode.window.visibleTextEditors) {
        if (editor.document === e.document) repaint(editor);
      }
    })
  );

  // Not awaited: activate() must return promptly so the extension host isn't blocked.
  client.start().then(() => vscode.window.visibleTextEditors.forEach(repaint));
}

function deactivate() {
  return client?.stop();
}

module.exports = { activate, deactivate };
