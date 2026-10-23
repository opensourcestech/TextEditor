"""Loading and running plugins: importing the packages' `.py` files, then
running their commands and telling their listeners about events.

Each package is importable as a Python package named after it, whether it
is a folder in Packages or a zipped package, so plugins can use relative
imports. The `.py` files at the top of a package are its plugins.
"""

import importlib
import importlib.abc
import importlib.util
import sys
import traceback

import _host
import texteditor
from texteditor import (
    ApplicationCommand,
    Edit,
    EventListener,
    ListInputHandler,
    TextCommand,
    ViewEventListener,
    WindowCommand,
    command_name,
)

application_command_classes = []
window_command_classes = []
text_command_classes = []
event_listener_classes = []
view_event_listener_classes = []

event_listeners = []
# View id -> {ViewEventListener class: instance}.
view_event_listeners = {}
plugin_modules = []

# Events TextEditor can send. Each also runs the `_async` version.
EVENTS = [
    "on_new",
    "on_load",
    "on_activated",
    "on_deactivated",
    "on_pre_save",
    "on_post_save",
    "on_modified",
    "on_selection_modified",
    "on_pre_close",
    "on_close",
    "on_query_context",
    "on_query_completions",
]


# ----- Finding package modules -----------------------------------------------------------


class _PackageLoader(importlib.abc.Loader):
    def __init__(self, package, relpath, source, is_package):
        self.package = package
        self.relpath = relpath
        self.source = source
        self.is_package = is_package

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        if self.is_package:
            module.__path__ = []
        if self.source is None:
            return
        origin = self.package.origin(self.relpath)
        module.__file__ = origin
        exec(compile(self.source, origin, "exec"), module.__dict__)


class _PackageFinder(importlib.abc.MetaPathFinder):
    """Imports `Package.module` from a package's folder or zip."""

    def find_spec(self, fullname, path, target=None):
        top, _, rest = fullname.partition(".")
        package = _host.find_package(top)
        if package is None:
            return None
        rel = rest.replace(".", "/")
        candidates = [(rel + "/__init__.py", True), (rel + ".py", False)] if rel else [("__init__.py", True)]
        for relpath, is_package in candidates:
            source = package.read(relpath)
            if source is not None:
                loader = _PackageLoader(package, relpath, source, is_package)
                return importlib.util.spec_from_loader(fullname, loader, origin=package.origin(relpath), is_package=is_package)
        # A folder without __init__.py, or the package itself: an empty package.
        if not rel or package.is_dir(rel):
            loader = _PackageLoader(package, rel, None, True)
            return importlib.util.spec_from_loader(fullname, loader, is_package=True)
        return None


_finder = _PackageFinder()


# ----- Loading -------------------------------------------------------------------------


def _register(module):
    bases = [
        (ApplicationCommand, application_command_classes),
        (WindowCommand, window_command_classes),
        (TextCommand, text_command_classes),
        (ViewEventListener, view_event_listener_classes),
        (EventListener, event_listener_classes),
    ]
    for value in list(vars(module).values()):
        if not isinstance(value, type):
            continue
        for base, found in bases:
            if issubclass(value, base) and value is not base and value not in found:
                found.append(value)
                if base is EventListener:
                    event_listeners.append(value())
                break


def load(message):
    """Imports every plugin. Returns their commands, the events they
    listen for, and any errors."""
    _host.info.clear()
    _host.info.update(message.get("info", {}))
    _host.packages[:] = [
        _host.Package(p["name"], p.get("folder"), p.get("archive"), p.get("root")) for p in message.get("packages", [])
    ]
    if _finder not in sys.meta_path:
        sys.meta_path.insert(0, _finder)
    for lib in message.get("lib_paths", []):
        if lib not in sys.path:
            sys.path.append(lib)

    errors = []
    for package in _host.packages:
        plugins = sorted(f[:-3] for f in package.files() if f.endswith(".py") and "/" not in f)
        for stem in plugins:
            try:
                module = importlib.import_module(package.name + "." + stem)
            except Exception:
                traceback.print_exc()
                errors.append("%s/%s.py: %s" % (package.name, stem, traceback.format_exc().strip().splitlines()[-1]))
                continue
            plugin_modules.append(module)
            _register(module)
    for module in plugin_modules:
        loaded = getattr(module, "plugin_loaded", None)
        if callable(loaded):
            try:
                loaded()
            except Exception:
                traceback.print_exc()

    commands = {}
    for kind, classes in (("application", application_command_classes), ("window", window_command_classes), ("text", text_command_classes)):
        for cls in classes:
            commands[command_name(cls.__name__)] = kind
    listened = set()
    for cls in event_listener_classes + view_event_listener_classes:
        for event in EVENTS:
            if hasattr(cls, event) or hasattr(cls, event + "_async"):
                listened.add(event)
    return {"commands": commands, "events": sorted(listened), "errors": errors}


# ----- Running commands ------------------------------------------------------------------


def _find(classes, name):
    for cls in classes:
        if command_name(cls.__name__) == name:
            return cls
    return None


def _call_run(command, args, edit_view=None):
    """Runs a command, first asking for missing arguments if it has input
    handlers, as the command palette does."""
    args = dict(args or {})
    if not command.is_enabled(**args):
        return
    handler = command.input(args)
    if handler is not None and handler.name() not in args:
        _ask(handler, lambda value: _call_run(command, dict(args, **{handler.name(): value}), edit_view))
        return
    if edit_view is not None:
        _host.edit_depth += 1
        edit_view._call("begin_edit")
        try:
            command.run(Edit(edit_view.view_id), **args)
        finally:
            _host.edit_depth -= 1
            edit_view._call("end_edit")
    else:
        command.run(**args)


def _ask(handler, on_value):
    window = texteditor.active_window()
    if isinstance(handler, ListInputHandler):
        items = handler.list_items()
        if isinstance(items, tuple):
            items = items[0]
        texts, values = [], []
        for item in items:
            if isinstance(item, texteditor.ListInputItem):
                texts.append(item.text)
                values.append(item.value)
            elif isinstance(item, (list, tuple)):
                texts.append(str(item[0]))
                values.append(item[1])
            else:
                texts.append(str(item))
                values.append(item)
        window.show_quick_panel(texts, lambda i: on_value(values[i]) if i >= 0 else handler.cancel(), placeholder=handler.placeholder())
    else:
        caption = handler.placeholder() or handler.name()
        window.show_input_panel(caption, handler.initial_text(), on_value, None, handler.cancel)


def run_text_command(view, name, args):
    """Runs a plugin text command; False if there is none by that name."""
    cls = _find(text_command_classes, name)
    if cls is None:
        return False
    _call_run(cls(view), args, edit_view=view)
    return True


def run_window_command(window, name, args):
    cls = _find(window_command_classes, name)
    if cls is None:
        return False
    _call_run(cls(window), args)
    return True


def run_application_command(name, args):
    cls = _find(application_command_classes, name)
    if cls is None:
        return False
    _call_run(cls(), args)
    return True


def run(message):
    """A command TextEditor asked for: from a key binding, menu or the palette."""
    name, args = message["name"], message.get("args") or {}
    view = texteditor.View(message["view"]) if message.get("view") else None
    if view is not None and run_text_command(view, name, args):
        return
    if run_window_command(texteditor.Window(1), name, args):
        return
    if run_application_command(name, args):
        return
    raise LookupError("no plugin command " + name)


# ----- Events -----------------------------------------------------------------------------


def _view_listeners(view):
    instances = view_event_listeners.setdefault(view.view_id, {})
    for cls in view_event_listener_classes:
        if cls not in instances:
            try:
                applies = cls.is_applicable(view.settings())
            except Exception:
                traceback.print_exc()
                applies = False
            instances[cls] = cls(view) if applies else None
    return [i for i in instances.values() if i is not None]


def dispatch(message):
    """Runs every listener for an event. For on_query_context, returns the
    first answer that isn't None; for on_query_completions, every
    listener's completions together."""
    event = message["event"]
    view = texteditor.View(message["view"]) if message.get("view") else None
    args = message.get("args") or []
    if event == "on_query_completions":
        return _completions(view, args, message.get("id"))
    answer = None
    for listener in event_listeners:
        method = getattr(listener, event, None)
        if method is not None:
            result = _safely(method, view, *args)
            if answer is None:
                answer = result
        later = getattr(listener, event + "_async", None)
        if later is not None:
            _host.schedule(lambda later=later: _safely(later, view, *args))
    if view is not None:
        for listener in _view_listeners(view):
            method = getattr(listener, event, None)
            if method is not None:
                result = _safely(method, *args)
                if answer is None:
                    answer = result
            later = getattr(listener, event + "_async", None)
            if later is not None:
                _host.schedule(lambda later=later: _safely(later, *args))
        if event == "on_close":
            view_event_listeners.pop(view.view_id, None)
    return answer


def _completions(view, args, request):
    """Every listener's completions. A CompletionList without its
    completions yet sends them to TextEditor when set_completions fills it."""
    items, flags, pending = [], 0, False
    listeners = [(l, (view,)) for l in event_listeners] + [(l, ()) for l in (_view_listeners(view) if view else [])]
    for listener, before in listeners:
        method = getattr(listener, "on_query_completions", None)
        if method is None:
            continue
        result = _safely(method, *(before + tuple(args)))
        if isinstance(result, texteditor.CompletionList) and result.completions is None:
            result._request = request
            pending = True
            continue
        found, more_flags = _completion_items(result)
        items.extend(found)
        flags |= more_flags
    return {"items": items, "flags": flags, "pending": pending}


def send_late_completions(completion_list):
    """A CompletionList that was given empty has been filled in."""
    found, flags = _completion_items(completion_list)
    _host.send({"t": "completions", "request": completion_list._request, "items": found, "flags": flags})


def _completion_items(result):
    """A listener's answer as plain data: a list of strings, of [trigger,
    contents] pairs, of CompletionItems, or a CompletionList; maybe with
    flags."""
    if result is None:
        return [], 0
    flags = 0
    if isinstance(result, texteditor.CompletionList):
        flags = result.flags
        result = result.completions or []
    elif isinstance(result, tuple) and len(result) == 2 and isinstance(result[1], int):
        result, flags = result
    items = []
    for c in result or []:
        if isinstance(c, texteditor.CompletionItem):
            items.append(
                {
                    "trigger": c.trigger,
                    "annotation": c.annotation,
                    "completion": c.completion,
                    "format": c.completion_format,
                    "args": c.args,
                }
            )
        elif isinstance(c, str):
            items.append({"trigger": c, "completion": c})
        elif isinstance(c, (list, tuple)) and len(c) >= 2:
            # The older ["trigger\tannotation", "snippet $1"] pairs.
            trigger, _, annotation = str(c[0]).partition("\t")
            items.append({"trigger": trigger, "annotation": annotation, "completion": str(c[1]), "format": 1})
    return items, flags


def _safely(function, *args):
    try:
        return function(*args)
    except Exception:
        traceback.print_exc()
        return None
