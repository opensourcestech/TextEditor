"""For Sublime Text's plugins: the `sublime` module they import.

It is TextEditor's own plugin API (`texteditor`) under Sublime Text's name,
so that plugins written for Sublime Text run as they are.
"""

import texteditor as _api
from texteditor import *  # noqa: F401,F403


def __getattr__(name):
    return getattr(_api, name)
