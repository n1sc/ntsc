"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const vscode = require("vscode");

async function poll(check, message) {
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline) {
        const value = await check();
        if (value) {
            return value;
        }
        await new Promise(resolve => setTimeout(resolve, 100));
    }
    throw new Error(message);
}

exports.run = async function() {
    const directory = await fs.mkdtemp(path.join(os.tmpdir(), "neutron-vscode "));
    const resultFile = process.env.NEUTRON_EDITOR_TEST_RESULT || path.join(os.tmpdir(), "neutron-vscode-host-result.json");
    try {
        const root = path.resolve(__dirname, "../../..");
        const configuration = vscode.workspace.getConfiguration("neutron");
        await configuration.update("serverPath", path.join(root, "target/debug/ntsc-lsp"), vscode.ConfigurationTarget.Global);
        await configuration.update("serverArgs", [], vscode.ConfigurationTarget.Global);
        await vscode.workspace.getConfiguration("editor").update("wordBasedSuggestions", "off", vscode.ConfigurationTarget.Global);
        await fs.writeFile(path.join(directory, "neutron.toml"), '[package]\nentry = "main.nt"\noutput = "test"\ncompiler = ">=0.1.0"\n');
        const source = "class Item { var int count; fun ping() {} } class Other { fun unrelated() {} } fun make() -> Item { return Item(); } fun main() { var item = make(); item. }";
        const file = path.join(directory, "main.nt");
        await fs.writeFile(file, source);
        const document = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
        await vscode.window.showTextDocument(document);
        assert.equal(document.languageId, "neutron");
        await vscode.extensions.getExtension("n1sc.neutron").activate();
        const position = document.positionAt(source.indexOf("item.") + "item.".length);
        const completions = await poll(async () => {
            const result = await vscode.commands.executeCommand("vscode.executeCompletionItemProvider", document.uri, position, ".");
            return result?.items.some(item => item.label === "ping") ? result : undefined;
        }, "VS Code did not receive typed member completions");
        assert.ok(completions.items.some(item => item.label === "count"));
        assert.ok(!completions.items.some(item => item.label === "unrelated"));
        const commands = await vscode.commands.getCommands(true);
        for (const name of ["build", "run", "test", "restartServer", "showOutput"]) {
            assert.ok(commands.includes(`neutron.${name}`));
        }
        await vscode.commands.executeCommand("neutron.restartServer");
        await poll(async () => {
            const result = await vscode.commands.executeCommand("vscode.executeCompletionItemProvider", document.uri, position, ".");
            return result?.items.some(item => item.label === "ping");
        }, "VS Code did not reconnect after restarting the server");
        console.log("VS Code extension-host integration passed");
        await fs.writeFile(resultFile, JSON.stringify({ status: "passed", timestamp: new Date().toISOString() }));
    } catch (error) {
        await fs.writeFile(resultFile, JSON.stringify({ status: "failed", error: String(error) }));
        throw error;
    } finally {
        await vscode.commands.executeCommand("workbench.action.closeAllEditors");
        await fs.rm(directory, { recursive: true, force: true });
    }
};
