#!/usr/bin/env node
/**
 * webprobe CLI shim.
 * Finds the webprobe binary (installed by postinstall via cargo) and delegates
 * all arguments to it.  Exits with the same code as the binary.
 */

'use strict';

const { execFileSync } = require('child_process');
const { join } = require('path');
const { existsSync } = require('fs');
const os = require('os');

function findBinary() {
    // 1. ~/.cargo/bin (default cargo install location)
    const cargoPath = join(os.homedir(), '.cargo', 'bin', 'webprobe');
    if (existsSync(cargoPath)) return cargoPath;

    // 2. Same directory as this script (for bundled distributions)
    const localPath = join(__dirname, 'webprobe');
    if (existsSync(localPath)) return localPath;

    // 3. Assume it's on PATH
    return 'webprobe';
}

const binary = findBinary();

try {
    execFileSync(binary, process.argv.slice(2), { stdio: 'inherit' });
} catch (/** @type {any} */ err) {
    if (err.status != null) {
        process.exit(err.status);
    }
    // Binary not found or couldn't exec
    console.error(
        '\nwebprobe: could not find the webprobe binary.\n' +
        'Make sure Rust / cargo is installed and run:\n' +
        '  cargo install --git https://github.com/KaiavN/WebProbe.git\n'
    );
    process.exit(1);
}
