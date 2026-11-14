"use strict";

const assert = require("node:assert/strict");
const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = require("node:fs");
const { tmpdir } = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { projectRoot, serverCommand, contract } = require("../src/project");

test("nearest project wins, including paths with spaces and nested projects", t => {
    const root = mkdtempSync(path.join(tmpdir(), "neutron editor "));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    const nested = path.join(root, "packages", "child");
    mkdirSync(path.join(nested, "src"), { recursive: true });
    writeFileSync(path.join(root, "neutron.toml"), "");
    writeFileSync(path.join(nested, "neutron.toml"), "");
    assert.equal(projectRoot(path.join(nested, "src", "main.nt")), nested);
    assert.equal(projectRoot(path.join(root, "src", "main.nt")), root);
    assert.equal(projectRoot(path.join(root, "..", "no-project.nt")), undefined);
});

test("standalone server arguments are explicit and not guessed from executable names", () => {
    const settings = { serverPath: "/path with spaces/ntsc-lsp", serverArgs: [] };
    assert.deepEqual(serverCommand({ get: (key, fallback) => settings[key] ?? fallback }), {
        command: settings.serverPath, args: [],
    });
    assert.deepEqual(serverCommand({ get: (_key, fallback) => fallback }), {
        command: contract.server.command, args: contract.server.args,
    });
});
