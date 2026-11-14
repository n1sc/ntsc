"use strict";

const fs = require("node:fs");
const path = require("node:path");
const contract = require("../contract.json");

function projectRoot(filePath) {
    let directory = path.dirname(filePath);
    for (;;) {
        if (fs.existsSync(path.join(directory, contract.language.projectMarker))) {
            return directory;
        }
        const parent = path.dirname(directory);
        if (parent === directory) {
            return undefined;
        }
        directory = parent;
    }
}

function serverCommand(configuration) {
    return {
        command: configuration.get("serverPath", contract.server.command),
        args: configuration.get("serverArgs", contract.server.args),
    };
}

module.exports = { projectRoot, serverCommand, contract };
