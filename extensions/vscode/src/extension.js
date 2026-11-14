"use strict";

const path = require("node:path");
const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");
const { projectRoot, serverCommand, contract } = require("./project");

let client;
let pending = Promise.resolve();
let output;

function report(error) {
    output.appendLine(String(error));
    void vscode.window.showErrorMessage(`Neutron: ${error.message || error}`);
}

function enqueue(action) {
    pending = pending.then(action).catch(report);
    return pending;
}

async function openDocument(document) {
    if (!vscode.workspace.isTrusted || document.languageId !== contract.language.id || document.uri.scheme !== "file") {
        return;
    }
    const root = vscode.workspace.getWorkspaceFolder(document.uri)?.uri.fsPath
        || projectRoot(document.uri.fsPath)
        || path.dirname(document.uri.fsPath);
    if (client) {
        return;
    }
    const configuration = vscode.workspace.getConfiguration("neutron");
    const command = serverCommand(configuration);
    command.options = { cwd: root };
    client = new LanguageClient(contract.server.id, "Neutron", command, {
        documentSelector: [{ scheme: "file", language: contract.language.id }],
        outputChannel: output,
    });
    try {
        await client.start();
    } catch (error) {
        await client.dispose();
        client = undefined;
        throw error;
    }
}

async function stopClients() {
    const running = client;
    client = undefined;
    if (running) {
        await running.dispose();
    }
}

async function restart() {
    await stopClients();
    for (const document of vscode.workspace.textDocuments) {
        await openDocument(document);
    }
}

function makeTask(action, root, scope) {
    const args = contract.workflows[action];
    if (!args) {
        throw new Error(`Unknown workflow: ${action}`);
    }
    const uri = vscode.Uri.file(root);
    const executable = vscode.workspace.getConfiguration("neutron", uri).get("cliPath", contract.cli);
    return new vscode.Task(
        { type: "neutron", action, project: root },
        scope,
        `Neutron: ${action[0].toUpperCase()}${action.slice(1)}`,
        "Neutron",
        new vscode.ProcessExecution(executable, args, { cwd: root }),
        [],
    );
}

async function workflow(action) {
    if (!vscode.workspace.isTrusted) {
        throw new Error("Trust the workspace before running the compiler.");
    }
    const document = vscode.window.activeTextEditor?.document;
    let root = document?.uri.scheme === "file" ? projectRoot(document.uri.fsPath) : undefined;
    let folder = document ? vscode.workspace.getWorkspaceFolder(document.uri) : undefined;
    if (!root) {
        const folders = vscode.workspace.workspaceFolders || [];
        folder = folders.length === 1 ? folders[0] : await vscode.window.showWorkspaceFolderPick();
        if (!folder) {
            return;
        }
        root = projectRoot(path.join(folder.uri.fsPath, "main.nt"));
    }
    if (!root) {
        throw new Error("Open a project containing neutron.toml first.");
    }
    if (!await vscode.workspace.saveAll(false)) {
        throw new Error("Could not save all source files; compiler was not started.");
    }
    await vscode.tasks.executeTask(makeTask(action, root, folder || vscode.TaskScope.Workspace));
}

function activate(context) {
    output = vscode.window.createOutputChannel("Neutron");
    context.subscriptions.push(output);
    for (const action of Object.keys(contract.workflows)) {
        context.subscriptions.push(vscode.commands.registerCommand(`neutron.${action}`, () => enqueue(() => workflow(action))));
    }
    context.subscriptions.push(
        vscode.commands.registerCommand("neutron.restartServer", () => enqueue(restart)),
        vscode.commands.registerCommand("neutron.showOutput", () => output.show()),
        vscode.workspace.onDidOpenTextDocument(document => enqueue(() => openDocument(document))),
        vscode.workspace.onDidChangeConfiguration(event => {
            if (event.affectsConfiguration("neutron")) {
                void enqueue(restart);
            }
        }),
        vscode.workspace.onDidChangeWorkspaceFolders(() => enqueue(restart)),
        vscode.workspace.onDidGrantWorkspaceTrust(() => enqueue(restart)),
        vscode.tasks.registerTaskProvider("neutron", {
            provideTasks() {
                return (vscode.workspace.workspaceFolders || []).flatMap(folder => {
                    const root = projectRoot(path.join(folder.uri.fsPath, "main.nt"));
                    return root ? Object.keys(contract.workflows).map(action => makeTask(action, root, folder)) : [];
                });
            },
            resolveTask(task) {
                const folder = task.scope;
                if (!folder?.uri || !contract.workflows[task.definition.action]) {
                    return undefined;
                }
                const root = path.resolve(folder.uri.fsPath, task.definition.project || ".");
                if (projectRoot(path.join(root, "main.nt")) !== root) {
                    return undefined;
                }
                return makeTask(task.definition.action, root, folder);
            },
        }),
    );
    void enqueue(restart);
}

async function deactivate() {
    await pending;
    await stopClients();
}

module.exports = { activate, deactivate };
