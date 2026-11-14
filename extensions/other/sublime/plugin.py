from LSP.plugin import AbstractPlugin, register_plugin, unregister_plugin
import sublime
import sublime_plugin
import json
import os

CONTRACT = json.loads(sublime.load_resource("Packages/LSP-neutron/contract.json"))


class NeutronPlugin(AbstractPlugin):
    @classmethod
    def name(cls):
        return "LSP-neutron"

    @classmethod
    def configuration(cls):
        return (
            sublime.load_settings("LSP-neutron.sublime-settings"),
            "Packages/LSP-neutron/LSP-neutron.sublime-settings",
        )


def plugin_loaded():
    register_plugin(NeutronPlugin)


def plugin_unloaded():
    unregister_plugin(NeutronPlugin)


class NeutronWorkflowCommand(sublime_plugin.WindowCommand):
    def run(self, action):
        arguments = CONTRACT["workflows"].get(action)
        if arguments is None:
            sublime.error_message("Neutron: unknown workflow: " + action)
            return
        view = self.window.active_view()
        roots = ([os.path.dirname(view.file_name())] if view and view.file_name()
                 else self.window.folders())
        if len(roots) != 1:
            sublime.error_message("Neutron: select a source file in the project first.")
            return
        root = roots[0]
        while not os.path.isfile(os.path.join(root, CONTRACT["language"]["projectMarker"])):
            parent = os.path.dirname(root)
            if parent == root:
                sublime.error_message("Neutron: open a project containing neutron.toml.")
                return
            root = parent
        self.window.run_command("save_all")
        if any(item.is_dirty() and item.file_name() for item in self.window.views()):
            sublime.error_message("Neutron: save modified files before running the compiler.")
            return
        settings = sublime.load_settings("Neutron.sublime-settings")
        self.window.run_command("exec", {
            "cmd": [settings.get("cli", CONTRACT["cli"])] + arguments,
            "working_dir": root,
        })
