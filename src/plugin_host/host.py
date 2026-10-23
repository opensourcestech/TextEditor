"""TextEditor's plugin host: runs packages' Python plugins in their own process.

TextEditor starts this with `python3 -I -u host.py` and talks to it over
stdin and stdout (see `_host`). It reads one message per line:

- `load`: import the plugins of the given packages; answered by `loaded`.
- `run`: run a command; answered by `done`.
- `event`: tell listeners about an event; answered by `done`.
- `callback`: call back a quick panel, input panel or dialog function.
- `reply`: the answer to one of our `call`s.
"""

import json
import os
import sys
import threading
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import _host  # noqa: E402

if sys.version_info < (3, 8):
    _host.send({"t": "loaded", "commands": {}, "events": [], "errors": ["Plugins need Python 3.8 or newer, not %d.%d" % sys.version_info[:2]]})
    sys.exit(1)

import texteditor  # noqa: E402,F401
import _plugins  # noqa: E402

sys.stdout = sys.stderr = _host.Console()


def read_messages():
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if message.get("t") == "reply":
            _host.reply_arrived(message)
        else:
            _host.work.put(("message", message))
    _host.work.put(("quit", None))


def handle(message):
    kind = message.get("t")
    if kind == "load":
        _host.send(dict(_plugins.load(message), t="loaded"))
        return
    if kind == "callback":
        callback = message.get("callback")
        function = _host.peek_callback(callback) if message.get("keep") else _host.take_callback(callback)
        if function is not None:
            function(*message.get("args", []))
        return
    done = {"t": "done", "id": message.get("id")}
    try:
        if kind == "run":
            _plugins.run(message)
        elif kind == "event":
            done["r"] = _plugins.dispatch(message)
    except Exception as e:
        traceback.print_exc()
        done["e"] = "%s: %s" % (type(e).__name__, e)
    finally:
        # A selection cleared and never added to stays as it was.
        _host.cleared_selections.clear()
        _host.send(done)


def main():
    threading.Thread(target=read_messages, daemon=True).start()
    while True:
        kind, item = _host.work.get()
        if kind == "quit":
            break
        try:
            if kind == "call":
                item()
            else:
                handle(item)
        except Exception:
            traceback.print_exc()
        sys.stdout.flush()


main()
