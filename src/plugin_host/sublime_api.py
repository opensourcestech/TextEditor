"""Sublime Text's private `sublime_api` module, which TextEditor doesn't
have. Plugins that import it still load; calling into it fails."""


def __getattr__(name):
    def missing(*args, **kwargs):
        raise NotImplementedError("sublime_api.%s isn't available in TextEditor" % name)

    return missing
