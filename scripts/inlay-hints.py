#!/usr/bin/env python3
"""Drive the server over raw stdio and report the `when:` hover per settings combination.

Verifies `ansibleLsp.inlayHints.enabled` — the one switch that gates the hover — without an
editor, so "the setting does nothing" can be answered with evidence instead of a guess.

    python3 scripts/inlay-hints.py [file]      # default: demo/playbook.yml

It asks the server for every reference range (`ansible/references`), hovers each one, and
counts which produce a `when:` explanation. The explanation moved from an inlay hint to a
hover (T-032); this script moved with it, and the filename is kept for continuity.

THE GOTCHA, which cost a debugging session: tower-lsp answers `-32002 Server not
initialized` to any request that arrives before it has processed the `initialized`
notification. Sending initialize/initialized/didOpen/hover in one burst looks like a dead
hover handler. A real client waits for the initialize *response* first, and so does this.
"""
import json, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = "ansible-lsp.exe" if sys.platform == "win32" else "ansible-lsp"
BIN = os.environ.get("ANSIBLE_LSP_BIN") or os.path.join(ROOT, "target", "release", EXE)


def frame(msg):
    body = json.dumps(msg).encode()
    return b"Content-Length: %d\r\n\r\n" % len(body) + body


class Server:
    def __init__(self, root, options):
        self.p = subprocess.Popen(
            [BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        self.id = 0
        self.send({"jsonrpc": "2.0", "id": self.next(), "method": "initialize", "params": {
            "processId": None, "rootUri": "file://" + root,
            "capabilities": {}, "initializationOptions": options}})
        if not self.wait(self.id):
            raise SystemExit("no initialize response")
        self.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

    def next(self):
        self.id += 1
        return self.id

    def send(self, msg):
        self.p.stdin.write(frame(msg))
        self.p.stdin.flush()

    def read_message(self):
        """One LSP frame, blocking. Portable — `select` on a pipe is Unix-only."""
        head = b""
        while b"\r\n\r\n" not in head:
            byte = self.p.stdout.read(1)
            if not byte:
                return None
            head += byte
        length = int(next(l for l in head.split(b"\r\n")
                          if b"Content-Length" in l).split(b":")[1])
        return json.loads(self.p.stdout.read(length))

    def wait(self, want_id):
        """Next response with this id, skipping the server's log-message notifications."""
        while True:
            msg = self.read_message()
            if msg is None:
                return None
            if msg.get("id") == want_id and "method" not in msg:
                return msg

    def request(self, method, params):
        rid = self.next()
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        msg = self.wait(rid)
        if msg is None:
            raise SystemExit(f"no {method} response")
        if msg.get("error"):
            raise SystemExit(f"{method} error: {msg['error']}")
        return msg["result"]

    def open(self, path):
        uri = "file://" + path
        self.send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri, "languageId": "yaml", "version": 1,
                             "text": open(path).read()}}})
        return uri

    def references(self, uri):
        return self.request("ansible/references", {"uri": uri}) or []

    def hover(self, uri, position):
        return self.request("textDocument/hover", {
            "textDocument": {"uri": uri}, "position": position})

    def kill(self):
        self.p.kill()


def one_line(hover):
    """Flatten hover markdown to a single readable line."""
    value = hover["contents"]["value"]
    return " ".join(value.replace("`", "").replace("**", "").split())


def main():
    target = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "demo", "playbook.yml")
    target = os.path.abspath(target)
    if not os.path.exists(BIN):
        raise SystemExit(f"{BIN} missing — run `cargo build --release`")

    print(f"{os.path.relpath(target, ROOT)}\n")
    for label, opts in [
        ("default", None),
        ("enabled=false", {"inlayHints": {"enabled": False}}),
    ]:
        s = Server(os.path.dirname(target), opts)
        uri = s.open(target)
        refs = s.references(uri)
        hovers = []
        for r in refs:
            h = s.hover(uri, r["range"]["start"])
            if h:
                hovers.append((r["range"]["start"]["line"], one_line(h)))
        s.kill()
        print(f"  {label:16s} {len(refs):3d} refs, {len(hovers):3d} with when: hover")
        if label == "default":
            for line, text in hovers[:5]:
                print(f"      L{line + 1:<4} {text[:70]}")


if __name__ == "__main__":
    main()
