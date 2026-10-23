"""The plugin host's connection to TextEditor.

Messages are JSON objects, one per line: TextEditor writes to our stdin and
reads our stdout. Plugins never see this module; `texteditor` and
`_plugins` are built on it.

Work (commands, events, timers) runs one item at a time on the main thread.
A reader thread hands TextEditor's replies to whichever
call is waiting for them.
"""

import io
import itertools
import json
import os
import queue
import sys
import threading
import zipfile

# The real stdout carries messages; `print` in plugins goes to the console.
_out = sys.stdout
_write_lock = threading.Lock()
_pending = {}
_ids = itertools.count(1)
_callbacks = {}
_callback_ids = itertools.count(1)

work = queue.Queue()

# Set by the "load" message: paths, platform and the packages.
info = {}
packages = []

# Views whose selection a plugin cleared and hasn't added to yet.
cleared_selections = set()

# How deep in TextCommand.run calls we are, for View.is_in_edit().
edit_depth = 0

CALL_TIMEOUT = 30


class ApiError(Exception):
    pass


def send(message):
    line = json.dumps(message, separators=(",", ":"))
    with _write_lock:
        _out.write(line + "\n")
        _out.flush()


def call(method, **params):
    """Asks TextEditor something and waits for the answer."""
    call_id = next(_ids)
    slot = {"event": threading.Event()}
    _pending[call_id] = slot
    send({"t": "call", "id": call_id, "m": method, "p": params})
    if not slot["event"].wait(CALL_TIMEOUT):
        _pending.pop(call_id, None)
        raise ApiError("TextEditor did not answer " + method)
    if slot.get("e"):
        raise ApiError(slot["e"])
    return slot.get("r")


def reply_arrived(message):
    slot = _pending.pop(message.get("id"), None)
    if slot is not None:
        slot["r"] = message.get("r")
        slot["e"] = message.get("e")
        slot["event"].set()


def register_callback(function):
    """An id TextEditor can use to call `function` back later."""
    if function is None:
        return None
    callback_id = next(_callback_ids)
    _callbacks[callback_id] = function
    return callback_id


def take_callback(callback_id):
    return _callbacks.pop(callback_id, None)


def peek_callback(callback_id):
    """A callback that can be called again, like a link's."""
    return _callbacks.get(callback_id)


def schedule(function, delay_ms=0):
    """Runs `function` on the main thread after `delay_ms`."""
    if delay_ms and delay_ms > 0:
        timer = threading.Timer(delay_ms / 1000.0, lambda: work.put(("call", function)))
        timer.daemon = True
        timer.start()
    else:
        work.put(("call", function))


class Console(io.TextIOBase):
    """Stands in for stdout and stderr: lines go to TextEditor's console."""

    def __init__(self):
        self._buffer = ""

    def writable(self):
        return True

    def write(self, text):
        self._buffer += text
        while "\n" in self._buffer:
            line, self._buffer = self._buffer.split("\n", 1)
            send({"t": "log", "text": line})
        return len(text)

    def flush(self):
        if self._buffer:
            send({"t": "log", "text": self._buffer})
            self._buffer = ""


# ----- Package files ---------------------------------------------------------


class Package:
    """A package's files: a folder in Packages, a zipped package, or
    both, in which case the folder's files win."""

    def __init__(self, name, folder, archive, root):
        self.name = name
        self.folder = folder
        self.archive = archive
        self.root = root or ""
        self._zip = None

    def _zipfile(self):
        if self._zip is None and self.archive:
            try:
                self._zip = zipfile.ZipFile(self.archive)
            except (OSError, zipfile.BadZipFile):
                self.archive = None
        return self._zip

    def read(self, relpath):
        """The bytes of a file in the package, or None."""
        if self.folder:
            path = os.path.join(self.folder, *relpath.split("/"))
            if os.path.isfile(path):
                with open(path, "rb") as f:
                    return f.read()
        archive = self._zipfile()
        if archive:
            try:
                return archive.read(self.root + relpath)
            except KeyError:
                pass
        return None

    def is_dir(self, relpath):
        if self.folder and os.path.isdir(os.path.join(self.folder, *relpath.split("/"))):
            return True
        archive = self._zipfile()
        prefix = self.root + relpath.rstrip("/") + "/"
        return bool(archive) and any(n.startswith(prefix) for n in archive.namelist())

    def files(self):
        """Paths of every file in the package, relative to it."""
        found = set()
        archive = self._zipfile()
        if archive:
            for name in archive.namelist():
                if name.startswith(self.root) and not name.endswith("/"):
                    found.add(name[len(self.root):])
        if self.folder:
            for dirpath, dirnames, filenames in os.walk(self.folder):
                dirnames[:] = [d for d in dirnames if not d.startswith(".")]
                rel = os.path.relpath(dirpath, self.folder)
                for f in filenames:
                    found.add(f if rel == "." else rel.replace(os.sep, "/") + "/" + f)
        return sorted(found)

    def origin(self, relpath):
        """Where a file lives, for tracebacks."""
        if self.folder:
            path = os.path.join(self.folder, *relpath.split("/"))
            if os.path.isfile(path):
                return path
        return os.path.join(self.archive or self.name, relpath)


def find_package(name):
    for package in packages:
        if package.name == name:
            return package
    return None


def read_resource(path):
    """Reads "Packages/Name/path" as bytes; raises IOError if missing."""
    parts = path.split("/", 2)
    if len(parts) == 3 and parts[0] == "Packages":
        package = find_package(parts[1])
        data = package.read(parts[2]) if package else None
        if data is not None:
            return data
    raise IOError("resource not found: " + path)


def resources():
    """Every "Packages/Name/path", in load order."""
    for package in packages:
        for relpath in package.files():
            yield "Packages/%s/%s" % (package.name, relpath)
