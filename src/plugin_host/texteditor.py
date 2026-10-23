"""The `texteditor` module: TextEditor's API for plugins.

Plugins are the `.py` files at the top of a package. They subclass the
command and listener classes here, and work with views, windows, regions,
selections, settings, resources, the clipboard, dialogs, quick and input
panels, output panels, status text, regions, phantoms, popups,
completions, layouts and groups, and timers:

    import texteditor

    class HelloCommand(texteditor.TextCommand):
        def run(self, edit):
            self.view.insert(edit, 0, "hello")

Sublime Text's plugins import the same API as `sublime` and
`sublime_plugin`, which are here for compatibility with them.
"""

import fnmatch
import json
import os
import re

import _host

# ----- Constants -------------------------------------------------------------

LITERAL = 1
IGNORECASE = 2
WHOLEWORD = 4
REVERSE = 8
WRAP = 16

MONOSPACE_FONT = 1
KEEP_OPEN_ON_FOCUS_LOST = 2
WANT_EVENT = 4

ENCODED_POSITION = 1
TRANSIENT = 4
FORCE_GROUP = 8
SEMI_TRANSIENT = 16
ADD_TO_SELECTION = 32
REPLACE_MRU = 64
CLEAR_TO_RIGHT = 128
FORCE_CLONE = 256

DRAW_EMPTY = 1
HIDE_ON_MINIMAP = 2
DRAW_EMPTY_AS_OVERWRITE = 4
PERSISTENT = 16
DRAW_OUTLINED = 32
DRAW_NO_FILL = 32
DRAW_NO_OUTLINE = 256
DRAW_SOLID_UNDERLINE = 512
DRAW_STIPPLED_UNDERLINE = 1024
DRAW_SQUIGGLY_UNDERLINE = 2048
HIDDEN = 128
NO_UNDO = 8192

OP_EQUAL = 0
OP_NOT_EQUAL = 1
OP_REGEX_MATCH = 2
OP_NOT_REGEX_MATCH = 3
OP_REGEX_CONTAINS = 4
OP_NOT_REGEX_CONTAINS = 5

CLASS_WORD_START = 1
CLASS_WORD_END = 2
CLASS_PUNCTUATION_START = 4
CLASS_PUNCTUATION_END = 8
CLASS_SUB_WORD_START = 16
CLASS_SUB_WORD_END = 32
CLASS_LINE_START = 64
CLASS_LINE_END = 128
CLASS_EMPTY_LINE = 256

INHIBIT_WORD_COMPLETIONS = 8
INHIBIT_EXPLICIT_COMPLETIONS = 16
DYNAMIC_COMPLETIONS = 32
INHIBIT_REORDER = 128

COMPLETION_FORMAT_TEXT = 0
COMPLETION_FORMAT_SNIPPET = 1
COMPLETION_FORMAT_COMMAND = 2

HOVER_TEXT = 1
HOVER_GUTTER = 2
HOVER_MARGIN = 3

LAYOUT_INLINE = 0
LAYOUT_BELOW = 1
LAYOUT_BLOCK = 2

DIALOG_CANCEL = 0
DIALOG_YES = 1
DIALOG_NO = 2

KIND_ID_AMBIGUOUS = 0
KIND_ID_KEYWORD = 1
KIND_ID_TYPE = 2
KIND_ID_FUNCTION = 3
KIND_ID_NAMESPACE = 4
KIND_ID_NAVIGATION = 5
KIND_ID_MARKUP = 6
KIND_ID_VARIABLE = 7
KIND_ID_SNIPPET = 8
KIND_ID_COLOR_REDISH = 9
KIND_ID_COLOR_ORANGISH = 10
KIND_ID_COLOR_YELLOWISH = 11
KIND_ID_COLOR_GREENISH = 12
KIND_ID_COLOR_CYANISH = 13
KIND_ID_COLOR_BLUISH = 14
KIND_ID_COLOR_PURPLISH = 15
KIND_ID_COLOR_PINKISH = 16
KIND_ID_COLOR_DARK = 17
KIND_ID_COLOR_LIGHT = 18
KIND_AMBIGUOUS = (KIND_ID_AMBIGUOUS, "", "")
KIND_KEYWORD = (1, "k", "Keyword")
KIND_TYPE = (2, "t", "Type")
KIND_FUNCTION = (3, "f", "Function")
KIND_NAMESPACE = (4, "a", "Namespace")
KIND_NAVIGATION = (5, "n", "Navigation")
KIND_MARKUP = (6, "m", "Markup")
KIND_VARIABLE = (7, "v", "Variable")
KIND_SNIPPET = (8, "s", "Snippet")


def __getattr__(name):
    """Constants this API doesn't have are 0, so plugins that only mention
    them still load."""
    if name.isupper():
        return 0
    raise AttributeError("module 'texteditor' has no attribute %r" % name)


# ----- The app -----------------------------------------------------------------


def version():
    return "4200"


def channel():
    return "stable"


def platform():
    return _host.info.get("platform", "osx")


def arch():
    return _host.info.get("arch", "x64")


def executable_path():
    return _host.info.get("executable", "")


def packages_path():
    return _host.info.get("packages_path", "")


def installed_packages_path():
    return _host.info.get("installed_packages_path", "")


def cache_path():
    return _host.info.get("cache_path", "")


def active_window():
    return Window(1)


def windows():
    return [Window(1)]


def status_message(message):
    _host.call("status_message", text=str(message))


def error_message(message):
    _host.call("dialog", kind="error", text=str(message))


def message_dialog(message):
    _host.call("dialog", kind="info", text=str(message))


def ok_cancel_dialog(message, ok_title="", title=""):
    return bool(_host.call("dialog", kind="ok_cancel", text=str(message), ok=ok_title))


def yes_no_cancel_dialog(message, yes_title="", no_title="", title=""):
    return _host.call("dialog", kind="yes_no_cancel", text=str(message), yes=yes_title, no=no_title)


def get_clipboard(size_limit=16777216):
    return _host.call("get_clipboard") or ""


def get_clipboard_async(callback, size_limit=16777216):
    _host.schedule(lambda: callback(get_clipboard()))


def set_clipboard(text):
    _host.call("set_clipboard", text=text)


def set_timeout(function, delay=0):
    _host.schedule(function, delay)


def set_timeout_async(function, delay=0):
    _host.schedule(function, delay)


def run_command(cmd, args=None):
    import _plugins

    if not _plugins.run_application_command(cmd, args):
        _host.call("run_command", name=cmd, args=args or {})


def score_selector(scope_name, selector):
    return _host.call("score_selector", scope=scope_name, selector=selector)


def open_dialog(callback, file_types=None, directory=None, multi_select=False, allow_folders=False):
    paths = _host.call("file_dialog", kind="open", directory=directory, multi=multi_select)
    _host.schedule(lambda: callback(paths if multi_select else (paths[0] if paths else None)))


def save_dialog(callback, file_types=None, directory=None, name=None, extension=None):
    path = _host.call("file_dialog", kind="save", directory=directory, name=name)
    _host.schedule(lambda: callback(path))


def select_folder_dialog(callback, directory=None, multi_select=False):
    paths = _host.call("file_dialog", kind="folder", directory=directory, multi=multi_select)
    _host.schedule(lambda: callback(paths if multi_select else (paths[0] if paths else None)))


def ui_info():
    return {"theme": {"name": ""}, "color_scheme": {"name": ""}}


def log_commands(flag=None):
    pass


def log_input(flag=None):
    pass


def log_result_regex(flag=None):
    pass


def log_indexing(flag=None):
    pass


def log_build_systems(flag=None):
    pass


def log_control_tree(flag=None):
    pass


def log_fps(flag=None):
    pass


# ----- Resources and settings files -------------------------------------------


def load_resource(name):
    return _host.read_resource(name).decode("utf-8")


def load_binary_resource(name):
    return _host.read_resource(name)


def find_resources(pattern):
    return [r for r in _host.resources() if fnmatch.fnmatch(r.rsplit("/", 1)[-1], pattern)]


def decode_value(text):
    """JSON with comments and trailing commas allowed."""
    out = []
    i, n, in_string = 0, len(text), False
    while i < n:
        c = text[i]
        if in_string:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 1
            elif c == '"':
                in_string = False
        elif text.startswith("//", i):
            end = text.find("\n", i)
            i = n if end < 0 else end
            continue
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            i = n if end < 0 else end + 2
            continue
        else:
            if c == '"':
                in_string = True
            out.append(c)
        i += 1
    cleaned = re.sub(r",(\s*[\]}])", r"\1", "".join(out))
    return json.loads(cleaned)


def encode_value(value, pretty=False):
    return json.dumps(value, indent=4 if pretty else None)


def expand_variables(value, variables):
    """Replaces `$name`, `${name}` and `${name:default}` in strings, lists and dicts."""
    if isinstance(value, str):

        def replace(m):
            name = m.group(1) or m.group(3)
            if name in variables:
                return str(variables[name])
            return m.group(2) or ""

        return re.sub(r"\$\{(\w+)(?::([^}]*))?\}|\$(\w+)", replace, value)
    if isinstance(value, list):
        return [expand_variables(v, variables) for v in value]
    if isinstance(value, dict):
        return {k: expand_variables(v, variables) for k, v in value.items()}
    return value


class Settings:
    """Settings a plugin reads and writes. `load_settings` gives ones merged
    from every package's file of that name (User last); `View.settings()`
    gives the view's, backed by TextEditor."""

    def __init__(self, values=None, view_id=None, file_name=None):
        self._values = values if values is not None else {}
        self._view_id = view_id
        self._file_name = file_name
        self._on_change = {}

    def get(self, key, default=None):
        if self._view_id is not None:
            value = _host.call("view_setting", view=self._view_id, key=key)
            return default if value is None else value
        return self._values.get(key, default)

    def set(self, key, value):
        if self._view_id is not None:
            _host.call("set_view_setting", view=self._view_id, key=key, value=value)
        else:
            self._values[key] = value
        for callback in list(self._on_change.values()):
            callback()

    def erase(self, key):
        if self._view_id is not None:
            self.set(key, None)
        else:
            self._values.pop(key, None)

    def has(self, key):
        return self.get(key) is not None

    def to_dict(self):
        return dict(self._values)

    def update(self, other=(), **kwargs):
        for k, v in dict(other, **kwargs).items():
            self.set(k, v)

    def setdefault(self, key, default=None):
        if not self.has(key):
            self.set(key, default)
        return self.get(key)

    def add_on_change(self, tag, callback):
        self._on_change[tag] = callback

    def clear_on_change(self, tag):
        self._on_change.pop(tag, None)

    def __getitem__(self, key):
        value = self.get(key)
        if value is None:
            raise KeyError(key)
        return value

    def __setitem__(self, key, value):
        self.set(key, value)

    def __delitem__(self, key):
        self.erase(key)

    def __contains__(self, key):
        return self.has(key)


_settings_files = {}


def load_settings(base_name):
    if base_name in _settings_files:
        return _settings_files[base_name]
    values = {}
    # The editor's own settings; Sublime Text's plugins ask for them by its name.
    if base_name in ("Preferences.te-settings", "Preferences.sublime-settings"):
        values.update(_host.call("app_settings") or {})
    for resource in find_resources(base_name):
        try:
            loaded = decode_value(load_resource(resource))
        except (ValueError, IOError):
            continue
        if isinstance(loaded, dict):
            values.update(loaded)
    settings = Settings(values, file_name=base_name)
    _settings_files[base_name] = settings
    return settings


def save_settings(base_name):
    settings = _settings_files.get(base_name)
    if settings is None:
        return
    path = os.path.join(packages_path(), "User", base_name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(settings.to_dict(), f, indent=4)


# ----- Regions -------------------------------------------------------------------


class Region:
    __slots__ = ("a", "b", "xpos")

    def __init__(self, a, b=None, xpos=-1):
        if b is None:
            b = a
        self.a = a
        self.b = b
        self.xpos = xpos

    def __repr__(self):
        return "Region(%d, %d)" % (self.a, self.b)

    __str__ = __repr__

    def __len__(self):
        return self.size()

    def __eq__(self, other):
        return isinstance(other, Region) and self.a == other.a and self.b == other.b

    def __hash__(self):
        return hash((self.a, self.b))

    def __lt__(self, other):
        return (self.begin(), self.end()) < (other.begin(), other.end())

    def __contains__(self, value):
        return self.contains(value)

    def __iter__(self):
        return iter((self.a, self.b))

    def to_tuple(self):
        return (self.a, self.b)

    def empty(self):
        return self.a == self.b

    def begin(self):
        return min(self.a, self.b)

    def end(self):
        return max(self.a, self.b)

    def size(self):
        return abs(self.a - self.b)

    def contains(self, x):
        if isinstance(x, Region):
            return self.begin() <= x.begin() and x.end() <= self.end()
        return self.begin() <= x <= self.end()

    def cover(self, region):
        return Region(min(self.begin(), region.begin()), max(self.end(), region.end()))

    def intersection(self, region):
        if not self.intersects(region):
            return Region(0)
        return Region(max(self.begin(), region.begin()), min(self.end(), region.end()))

    def intersects(self, region):
        if self.empty() or region.empty():
            return self.contains(region.begin()) or region.contains(self.begin())
        return self.begin() < region.end() and region.begin() < self.end()


def _region(value):
    return value if isinstance(value, Region) else Region(value)


def _regions(pairs):
    return [Region(a, b) for a, b in pairs or []]


class Selection:
    """A view's selections, read from TextEditor each time."""

    def __init__(self, view_id):
        self.view_id = view_id

    def _get(self):
        if self.view_id in _host.cleared_selections:
            return []
        return _regions(_host.call("sel", view=self.view_id))

    def _set(self, regions):
        _host.cleared_selections.discard(self.view_id)
        _host.call("set_sel", view=self.view_id, regions=[[r.a, r.b] for r in regions])

    def __len__(self):
        return len(self._get())

    def __getitem__(self, index):
        return self._get()[index]

    def __iter__(self):
        return iter(self._get())

    def __eq__(self, other):
        return isinstance(other, Selection) and list(self) == list(other)

    def __repr__(self):
        return "Selection(%r)" % self._get()

    def is_valid(self):
        return True

    def clear(self):
        _host.cleared_selections.add(self.view_id)

    def add(self, region):
        self._set(self._get() + [_region(region)])

    def add_all(self, regions):
        self._set(self._get() + [_region(r) for r in regions])

    def subtract(self, region):
        result = []
        for r in self._get():
            if not r.intersects(region):
                result.append(r)
                continue
            if r.begin() < region.begin():
                result.append(Region(r.begin(), region.begin()))
            if region.end() < r.end():
                result.append(Region(region.end(), r.end()))
        self._set(result)

    def contains(self, region):
        return any(r.contains(region) for r in self._get())


# ----- Views and windows ------------------------------------------------------------


class Syntax:
    def __init__(self, path, name, hidden=False, scope=""):
        self.path = path
        self.name = name
        self.hidden = hidden
        self.scope = scope

    def __eq__(self, other):
        return isinstance(other, Syntax) and self.path == other.path

    def __repr__(self):
        return "Syntax(%r, %r, %r, %r)" % (self.path, self.name, self.hidden, self.scope)


def _syntax(found):
    return Syntax(found["path"], found["name"], False, found["scope"]) if found else None


def list_syntaxes():
    return [_syntax(s) for s in _host.call("list_syntaxes")]


def find_syntax_by_name(name):
    return [s for s in list_syntaxes() if s.name == name]


def find_syntax_by_scope(scope):
    return [s for s in list_syntaxes() if s.scope == scope]


def find_syntax_for_file(path, first_line=""):
    return _syntax(_host.call("syntax_for_file", path=path, first_line=first_line))


def _window_views():
    return [View(i) for i in _host.call("views")]


def syntax_from_path(path):
    for s in list_syntaxes():
        if s.path == path:
            return s
    return None


class Buffer:
    def __init__(self, buffer_id):
        self.buffer_id = buffer_id

    def id(self):
        return self.buffer_id

    def file_name(self):
        return self.primary_view().file_name()

    def views(self):
        return [v for v in _window_views() if v.buffer_id() == self.buffer_id]

    def primary_view(self):
        views = self.views()
        return views[0] if views else View(self.buffer_id)


class View:
    def __init__(self, view_id):
        self.view_id = view_id

    def __repr__(self):
        return "View(%d)" % self.view_id

    def __eq__(self, other):
        return isinstance(other, View) and self.view_id == other.view_id

    def __hash__(self):
        return hash(self.view_id)

    def __bool__(self):
        return self.view_id > 0 and self.is_valid()

    def __len__(self):
        return self.size()

    def _call(self, method, **params):
        return _host.call(method, view=self.view_id, **params)

    def id(self):
        return self.view_id

    def buffer_id(self):
        return self._call("buffer_id")

    def buffer(self):
        return Buffer(self.buffer_id())

    def is_valid(self):
        return bool(self._call("view_valid"))

    def is_primary(self):
        return True

    def window(self):
        window_id = self._call("view_window")
        return Window(window_id) if window_id else None

    def file_name(self):
        return self._call("file_name")

    def name(self):
        return self._call("view_name") or ""

    def set_name(self, name):
        self._call("set_view_name", name=name)

    def close(self):
        return bool(self._call("close_view"))

    def is_loading(self):
        return False

    def is_dirty(self):
        return bool(self._call("is_dirty"))

    def is_read_only(self):
        return bool(self._call("is_read_only"))

    def set_read_only(self, value):
        self._call("set_read_only", value=bool(value))

    def is_scratch(self):
        return bool(self._call("is_scratch"))

    def set_scratch(self, value):
        self._call("set_scratch", value=bool(value))

    def is_in_edit(self):
        return _host.edit_depth > 0

    def element(self):
        return self._call("view_element")

    def size(self):
        return self._call("size")

    def substr(self, x):
        if isinstance(x, Region):
            return self._call("substr", a=x.begin(), b=x.end())
        return self._call("substr", a=x, b=x + 1)

    def insert(self, edit, pt, text):
        self._call("insert", pt=pt, text=text)
        return len(text)

    def erase(self, edit, region):
        self._call("replace", a=region.begin(), b=region.end(), text="")

    def replace(self, edit, region, text):
        self._call("replace", a=region.begin(), b=region.end(), text=text)

    def change_count(self):
        return self._call("change_count")

    def run_command(self, cmd, args=None):
        import _plugins

        if not _plugins.run_text_command(self, cmd, args):
            self._call("run_command", name=cmd, args=args or {})

    def sel(self):
        return Selection(self.view_id)

    def line(self, x):
        r = _region(x)
        return Region(*self._call("line", a=r.begin(), b=r.end()))

    def full_line(self, x):
        r = _region(x)
        return Region(*self._call("full_line", a=r.begin(), b=r.end()))

    def lines(self, region):
        return _regions(self._call("lines", a=region.begin(), b=region.end()))

    def split_by_newlines(self, region):
        return [Region(max(r.begin(), region.begin()), min(r.end(), region.end())) for r in self.lines(region)]

    def word(self, x):
        r = _region(x)
        return Region(*self._call("word", a=r.begin(), b=r.end()))

    def classify(self, pt):
        return self._call("classify", pt=pt)

    def rowcol(self, pt):
        return tuple(self._call("rowcol", pt=pt))

    rowcol_utf8 = rowcol
    rowcol_utf16 = rowcol

    def text_point(self, row, col, clamp_column=False):
        return self._call("text_point", row=row, col=col, clamp=bool(clamp_column))

    text_point_utf8 = text_point
    text_point_utf16 = text_point

    def find(self, pattern, start_pt, flags=0):
        found = self._call("find", pattern=pattern, start=start_pt, flags=flags)
        return Region(*found) if found else Region(-1, -1)

    def find_all(self, pattern, flags=0, fmt=None, extractions=None):
        regions = _regions(self._call("find_all", pattern=pattern, flags=flags))
        if fmt is not None and extractions is not None:
            python_fmt = re.sub(r"\$(\d+)|\$\{(\d+)\}", lambda m: "\\g<%s>" % (m.group(1) or m.group(2)), fmt)
            regex = re.compile(re.escape(pattern) if flags & LITERAL else pattern, re.I if flags & IGNORECASE else 0)
            for r in regions:
                extractions.append(regex.sub(python_fmt, self.substr(r), count=1))
        return regions

    def settings(self):
        return Settings(view_id=self.view_id)

    def meta_info(self, key, pt):
        return None

    def scope_name(self, pt):
        return self._call("scope_name", pt=pt)

    def match_selector(self, pt, selector):
        return self.score_selector(pt, selector) > 0

    def score_selector(self, pt, selector):
        return score_selector(self.scope_name(pt), selector)

    def extract_scope(self, pt):
        return Region(*self._call("extract_scope", pt=pt))

    def syntax(self):
        return _syntax(self._call("syntax"))

    def set_syntax_file(self, path):
        self._call("set_syntax", path=path)

    def assign_syntax(self, syntax):
        self.set_syntax_file(syntax.path if isinstance(syntax, Syntax) else syntax)

    def indentation_level(self, pt):
        line = self.substr(self.line(pt))
        tab_size = self.settings().get("tab_size", 4)
        width = len(line.expandtabs(tab_size)) - len(line.expandtabs(tab_size).lstrip())
        return width // max(tab_size, 1)

    def symbols(self):
        return []

    def indexed_symbols(self):
        return []

    def indexed_references(self):
        return []

    def set_status(self, key, value):
        self._call("set_status", key=key, value=str(value))

    def get_status(self, key):
        return self._call("get_status", key=key) or ""

    def erase_status(self, key):
        self._call("set_status", key=key, value=None)

    def add_regions(self, key, regions, scope="", icon="", flags=0, *args, **kwargs):
        self._call("add_regions", key=key, regions=[[r.a, r.b] for r in regions], scope=scope, flags=flags)

    def get_regions(self, key):
        return _regions(self._call("get_regions", key=key))

    def erase_regions(self, key):
        self._call("add_regions", key=key, regions=[], scope="", flags=0)

    def visible_region(self):
        return Region(*self._call("visible_region"))

    def show(self, x, show_surrounds=True, keep_to_left=False, animate=True):
        r = _region(x) if not isinstance(x, Selection) else (list(x) or [Region(0)])[0]
        self._call("show", a=r.begin(), b=r.end(), center=False)

    def show_at_center(self, x, animate=True):
        r = _region(x)
        self._call("show", a=r.begin(), b=r.end(), center=True)

    def viewport_position(self):
        return tuple(self._call("viewport_position"))

    def set_viewport_position(self, xy, animate=True):
        self._call("set_viewport_position", x=xy[0], y=xy[1])

    def viewport_extent(self):
        return tuple(self._call("viewport_extent"))

    def layout_extent(self):
        return tuple(self._call("layout_extent"))

    def text_to_layout(self, pt):
        return tuple(self._call("text_to_layout", pt=pt))

    def text_to_window(self, pt):
        return tuple(self._call("text_to_window", pt=pt))

    def layout_to_text(self, xy):
        return self._call("layout_to_text", x=xy[0], y=xy[1])

    def window_to_text(self, xy):
        return self._call("window_to_text", x=xy[0], y=xy[1])

    def line_height(self):
        return self._call("line_height")

    def em_width(self):
        return self._call("em_width")

    def line_endings(self):
        return self._call("line_endings")

    def set_line_endings(self, line_ending):
        pass

    def encoding(self):
        return "UTF-8"

    def set_encoding(self, encoding):
        pass

    def is_auto_complete_visible(self):
        return bool(self._call("auto_complete_visible"))

    def is_popup_visible(self):
        return bool(self._call("popup_visible"))

    def show_popup(self, content, flags=0, location=-1, max_width=320, max_height=240, on_navigate=None, on_hide=None):
        self._call(
            "show_popup",
            content=str(content.text if isinstance(content, Html) else content),
            location=location,
            max_width=max_width,
            max_height=max_height,
            navigate=_host.register_callback(on_navigate),
            hide=_host.register_callback(on_hide),
        )

    def update_popup(self, content):
        self._call("update_popup", content=str(content.text if isinstance(content, Html) else content))

    def hide_popup(self):
        self._call("hide_popup")

    def add_phantom(self, key, region, content, layout=LAYOUT_INLINE, on_navigate=None):
        r = _region(region)
        return self._call(
            "add_phantom",
            key=key,
            a=r.begin(),
            b=r.end(),
            content=str(content.text if isinstance(content, Html) else content),
            layout=layout,
            navigate=_host.register_callback(on_navigate),
        )

    def erase_phantoms(self, key):
        self._call("erase_phantoms", key=key)

    def erase_phantom_by_id(self, pid):
        self._call("erase_phantom_by_id", id=pid)

    def query_phantom(self, pid):
        return [Region(*r) for r in self._call("query_phantoms", ids=[pid]) if r]

    def query_phantoms(self, pids):
        return [Region(*r) if r else Region(-1) for r in self._call("query_phantoms", ids=list(pids))]

    def folded_regions(self):
        return []

    def fold(self, x):
        return False

    def unfold(self, x):
        return []

    def get_overwrite_status(self):
        return False

    def set_overwrite_status(self, value):
        pass

    def command_history(self, index, modifying_only=False):
        return ("", None, 0)

    def style(self):
        return {}

    def style_for_scope(self, scope):
        return {}

    def clones(self):
        return []

    def change_id(self):
        return (self.change_count(), 0, 0)


class Window:
    def __init__(self, window_id):
        self.window_id = window_id
        self._settings = Settings()

    def __eq__(self, other):
        return isinstance(other, Window) and self.window_id == other.window_id

    def __hash__(self):
        return hash(self.window_id)

    def __bool__(self):
        return True

    def __repr__(self):
        return "Window(%d)" % self.window_id

    def id(self):
        return self.window_id

    def is_valid(self):
        return True

    def hwnd(self):
        return 0

    def active_view(self):
        view_id = _host.call("active_view")
        return View(view_id) if view_id else None

    def views(self, include_transient=False):
        return [View(i) for i in _host.call("views")]

    def num_groups(self):
        return _host.call("num_groups")

    def active_group(self):
        return _host.call("active_group")

    def focus_group(self, group):
        _host.call("focus_group", group=group)

    def views_in_group(self, group):
        return [View(i) for i in _host.call("views_in_group", group=group)]

    def active_view_in_group(self, group):
        view_id = _host.call("active_view_in_group", group=group)
        return View(view_id) if view_id else None

    def get_layout(self):
        return _host.call("get_layout")

    layout = get_layout

    def set_layout(self, layout):
        _host.call("set_layout", layout=layout)

    def sheets(self):
        return []

    def active_sheet(self):
        return None

    def get_view_index(self, view):
        return tuple(_host.call("get_view_index", view=view.view_id))

    def set_view_index(self, view, group, index):
        _host.call("set_view_index", view=view.view_id, group=group, index=index)

    def focus_view(self, view):
        _host.call("focus_view", view=view.view_id)

    def new_file(self, flags=0, syntax=""):
        return View(_host.call("new_file", syntax=syntax))

    def open_file(self, fname, flags=0, group=-1):
        return View(_host.call("open_file", path=fname, encoded=bool(flags & ENCODED_POSITION)))

    def find_open_file(self, fname):
        view_id = _host.call("find_open_file", path=fname)
        return View(view_id) if view_id else None

    def run_command(self, cmd, args=None):
        import _plugins

        if not _plugins.run_window_command(self, cmd, args):
            _host.call("run_command", name=cmd, args=args or {})

    def show_quick_panel(self, items, on_select, flags=0, selected_index=-1, on_highlight=None, placeholder=""):
        rows = []
        for item in items:
            if isinstance(item, QuickPanelItem):
                rows.append([item.trigger, item.details if isinstance(item.details, str) else " ".join(item.details)])
            elif isinstance(item, (list, tuple)):
                rows.append([str(item[0]), " ".join(str(x) for x in item[1:])])
            else:
                rows.append([str(item), ""])
        callback = _host.register_callback(on_select)
        _host.call("quick_panel", items=rows, callback=callback, selected=selected_index, placeholder=placeholder or "")

    def show_input_panel(self, caption, initial_text, on_done, on_change, on_cancel):
        _host.call(
            "input_panel",
            caption=caption,
            text=initial_text,
            done=_host.register_callback(on_done),
            cancel=_host.register_callback(on_cancel),
        )
        return View(-1)

    def create_output_panel(self, name, unlisted=False):
        return View(_host.call("create_output_panel", name=name))

    get_output_panel = create_output_panel

    def find_output_panel(self, name):
        view_id = _host.call("find_output_panel", name=name)
        return View(view_id) if view_id else None

    def destroy_output_panel(self, name):
        _host.call("destroy_output_panel", name=name)

    def active_panel(self):
        return _host.call("active_panel")

    def panels(self):
        return _host.call("panels")

    def folders(self):
        return _host.call("folders")

    def project_file_name(self):
        return None

    def project_data(self):
        folders = self.folders()
        return {"folders": [{"path": f} for f in folders]} if folders else None

    def set_project_data(self, data):
        pass

    def workspace_file_name(self):
        return None

    def settings(self):
        return self._settings

    template_settings = settings

    def extract_variables(self):
        variables = {"packages": packages_path(), "platform": {"osx": "OSX", "linux": "Linux", "windows": "Windows"}[platform()]}
        view = self.active_view()
        path = view.file_name() if view else None
        if path:
            base = os.path.basename(path)
            variables.update(
                file=path,
                file_path=os.path.dirname(path),
                file_name=base,
                file_base_name=os.path.splitext(base)[0],
                file_extension=os.path.splitext(base)[1].lstrip("."),
            )
        folders = self.folders()
        if folders:
            variables["folder"] = folders[0]
        return variables

    def status_message(self, message):
        status_message(message)

    def is_sidebar_visible(self):
        return bool(_host.call("sidebar_visible"))

    def set_sidebar_visible(self, flag, animate=True):
        _host.call("set_sidebar", visible=bool(flag))

    def is_minimap_visible(self):
        return False

    def set_minimap_visible(self, flag):
        pass

    def is_status_bar_visible(self):
        return True

    def set_status_bar_visible(self, flag):
        pass

    def get_tabs_visible(self):
        return True

    def set_tabs_visible(self, flag):
        pass

    def is_menu_visible(self):
        return True

    def set_menu_visible(self, flag):
        pass

    def lookup_symbol_in_index(self, symbol):
        return []

    def lookup_symbol_in_open_files(self, symbol):
        return []

    def lookup_references_in_index(self, symbol):
        return []

    def lookup_references_in_open_files(self, symbol):
        return []


# ----- Things plugins create that this editor doesn't show ------------------------


class QuickPanelItem:
    def __init__(self, trigger, details="", annotation="", kind=KIND_AMBIGUOUS):
        self.trigger = trigger
        self.details = details
        self.annotation = annotation
        self.kind = kind


class ListInputItem:
    def __init__(self, text, value, details="", annotation="", kind=KIND_AMBIGUOUS):
        self.text = text
        self.value = value
        self.details = details
        self.annotation = annotation
        self.kind = kind


class CompletionItem:
    def __init__(self, trigger, annotation="", completion="", completion_format=COMPLETION_FORMAT_TEXT, kind=KIND_AMBIGUOUS, details=""):
        self.trigger = trigger
        self.annotation = annotation
        self.completion = completion or trigger
        self.completion_format = completion_format
        self.kind = kind
        self.details = details
        self.args = None

    @classmethod
    def snippet_completion(cls, trigger, snippet, annotation="", kind=KIND_SNIPPET, details=""):
        return cls(trigger, annotation, snippet, COMPLETION_FORMAT_SNIPPET, kind, details)

    @classmethod
    def command_completion(cls, trigger, command, args=None, annotation="", kind=KIND_AMBIGUOUS, details=""):
        item = cls(trigger, annotation, command, COMPLETION_FORMAT_COMMAND, kind, details)
        item.args = args or {}
        return item


class CompletionList:
    """Completions, maybe given later with set_completions."""

    def __init__(self, completions=None, flags=0):
        self.completions = completions
        self.flags = flags
        self._request = None

    def set_completions(self, completions, flags=0):
        self.completions = completions
        self.flags = flags
        if self._request is not None:
            import _plugins

            _plugins.send_late_completions(self)
            self._request = None


class Phantom:
    def __init__(self, region, content, layout, on_navigate=None):
        self.region = region
        self.content = content
        self.layout = layout
        self.on_navigate = on_navigate
        self.id = None

    def __eq__(self, other):
        return isinstance(other, Phantom) and (self.region, self.content, self.layout) == (other.region, other.content, other.layout)


class PhantomSet:
    """Phantoms that are replaced together."""

    def __init__(self, view, key=""):
        self.view = view
        self.key = key or "phantom_set_%d" % id(self)
        self.phantoms = []

    def update(self, phantoms):
        self.view.erase_phantoms(self.key)
        for phantom in phantoms:
            phantom.id = self.view.add_phantom(self.key, phantom.region, phantom.content, phantom.layout, phantom.on_navigate)
        self.phantoms = list(phantoms)


class Html:
    def __init__(self, text, base_dir=None):
        self.text = text


# ----- Commands and listeners ------------------------------------------------------

def command_name(class_name):
    """`FooBarCommand` → `foo_bar`: how a command class is named as a command."""
    name = class_name[0].lower()
    last_upper = False
    for c in class_name[1:]:
        if c.isupper() and not last_upper:
            name += "_" + c.lower()
        else:
            name += c
        last_upper = c.isupper()
    if name.endswith("_command"):
        name = name[: -len("_command")]
    return name


def split_identifier(text):
    """The words of an identifier: `parseHTTPResponse_code` gives parse,
    HTTP, Response and code."""
    words, word = [], ""
    for i, c in enumerate(text):
        if c in " \t_-":
            words.append(word)
            word = ""
            continue
        before = text[i - 1] if i else ""
        after = text[i + 1] if i + 1 < len(text) else ""
        # A capital starts a word after a lower-case letter, or ends a run of
        # capitals when a lower-case letter follows it ("HTTPResponse").
        if c.isupper() and word and (before.islower() or (before.isupper() and after.islower())):
            words.append(word)
            word = ""
        word += c
    words.append(word)
    return [w for w in words if w]


# ----- Base classes ------------------------------------------------------------------


class Command:
    def name(self):
        return command_name(self.__class__.__name__)

    def is_enabled(self, *args, **kwargs):
        return True

    def is_visible(self, *args, **kwargs):
        return True

    def is_checked(self, *args, **kwargs):
        return False

    def description(self, *args, **kwargs):
        return None

    def want_event(self):
        return False

    def input(self, args):
        return None

    def input_description(self):
        return ""


class ApplicationCommand(Command):
    pass


class WindowCommand(Command):
    def __init__(self, window):
        self.window = window


class TextCommand(Command):
    def __init__(self, view):
        self.view = view


class EventListener:
    pass


class ViewEventListener:
    def __init__(self, view):
        self.view = view

    @classmethod
    def is_applicable(cls, settings):
        return True

    @classmethod
    def applies_to_primary_view_only(cls):
        return True


class TextChangeListener:
    def attach(self, buffer):
        self.buffer = buffer

    def detach(self):
        pass

    def is_attached(self):
        return False

    @classmethod
    def is_applicable(cls, buffer):
        return False


class CommandInputHandler:
    def name(self):
        return command_name(self.__class__.__name__.replace("InputHandler", ""))

    def placeholder(self):
        return ""

    def initial_text(self):
        return ""

    def preview(self, value):
        return ""

    def validate(self, value):
        return True

    def cancel(self):
        pass

    def confirm(self, value):
        pass

    def next_input(self, args):
        return None

    def want_event(self):
        return False


class BackInputHandler(CommandInputHandler):
    pass


class TextInputHandler(CommandInputHandler):
    def description(self, text):
        return text


class ListInputHandler(CommandInputHandler):
    def list_items(self):
        return []

    def description(self, value, text):
        return text


class Edit:
    """The token a TextCommand gets to make its edits with."""

    def __init__(self, view_id):
        self.edit_token = view_id
