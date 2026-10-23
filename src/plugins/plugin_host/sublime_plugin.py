"""For Sublime Text's plugins: the `sublime_plugin` module they import.

It is TextEditor's command and listener classes (`texteditor`) under
Sublime Text's name, so that plugins written for Sublime Text run as they
are.
"""

import _plugins as _impl
from texteditor import (  # noqa: F401
    ApplicationCommand,
    BackInputHandler,
    CommandInputHandler,
    EventListener,
    ListInputHandler,
    TextChangeListener,
    TextCommand,
    TextInputHandler,
    ViewEventListener,
    WindowCommand,
    command_name,
    split_identifier,
)


def __getattr__(name):
    return getattr(_impl, name)
