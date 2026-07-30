#!/usr/bin/env python3
"""Drive the server over raw stdio and report inlay hints per settings combination.

Verifies `ansibleLsp.inlayHints.*` without an editor, so "the setting does nothing" can
be answered with evidence instead of a guess.

    python3 scripts/inlay-hints.py [file]      # default: demo/playbook.yml

THE GOTCHA, which cost a debugging session: tower-lsp answers `-32002 Server not
initialized` to any request that arrives before it has processed the `initialized`
notification. Sending initialize/initialized/didOpen/inlayHint in one burst looks like a
dead inlay-hint handler. A real client waits for the initialize *response* first, and so
does this.
"""
import json, os, select, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "ansible-lsp")


def frame(msg):
    body = json.dumps(msg).encode()
    return b"Content-Length: %d\r\n\r\n" % len(body) + body


class Server:
    def __init__(self, root, options):
        self.p = subprocess.Popen(
            [BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        self.buf = b""
        self.send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "processId": None, "rootUri": "file://" + root,
            "capabilities": {}, "initializationOptions": options}})
        if not self.wait(1):
            raise SystemExit("no initialize response")
        self.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

    def send(self, msg):
        self.p.stdin.write(frame(msg))
        self.p.stdin.flush()

    def wait(self, want_id, deadline=20):
        end = time.time() + deadline
        while time.time() < end:
            ready, _, _ = select.select([self.p.stdout], [], [], 0.2)
            if ready:
                self.buf += os.read(self.p.stdout.fileno(), 65536)
            while b"\r\n\r\n" in self.buf:
                head, rest = self.buf.split(b"\r\n\r\n", 1)
                length = int(next(l for l in head.split(b"\r\n")
                                  if b"Content-Length" in l).split(b":")[1])
                if len(rest) < length:
                    break
                body, self.buf = rest[:length], rest[length:]
                msg = json.loads(body)
                if msg.get("id") == want_id and "method" not in msg:
                    return msg
        return None

    def hints(self, path):
        uri = "file://" + path
        self.send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri, "languageId": "yaml", "version": 1,
                             "text": open(path).read()}}})
        self.send({"jsonrpc": "2.0", "id": 2, "method": "textDocument/inlayHint", "params": {
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 0, "character": 0},
                      "end": {"line": 10000, "character": 0}}}})
        msg = self.wait(2)
        if msg is None:
            raise SystemExit("no inlayHint response")
        if msg.get("error"):
            raise SystemExit(f"inlayHint error: {msg['error']}")
        return msg["result"] or []

    def kill(self):
        self.p.kill()


def main():
    target = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "demo", "playbook.yml")
    target = os.path.abspath(target)
    if not os.path.exists(BIN):
        raise SystemExit(f"{BIN} missing — run `cargo build --release`")

    print(f"{os.path.relpath(target, ROOT)}\n")
    for label, opts in [
        ("default", None),
        ("explanations=false", {"inlayHints": {"explanations": False}}),
        ("enabled=false", {"inlayHints": {"enabled": False}}),
    ]:
        s = Server(os.path.dirname(target), opts)
        hints = s.hints(target)
        s.kill()
        tips = sum(1 for h in hints if h.get("tooltip"))
        print(f"  {label:20s} {len(hints):3d} hints, {tips:3d} with tooltip")
        if label == "default":
            for h in hints[:5]:
                print(f"      L{h['position']['line'] + 1:<4} {h['label'].strip()}")


if __name__ == "__main__":
    main()
