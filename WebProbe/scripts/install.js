#!/usr/bin/env node
/**
 * postinstall script: install the webprobe binary via cargo.
 *
 * Runs automatically after `npm install -g webprobe-cli`.
 * Gracefully skips if cargo is not available (shows an installation hint).
 */

'use strict';

const { execSync, spawnSync } = require('child_process');
const os = require('os');

function hasCargo() {
    try {
        execSync('cargo --version', { stdio: 'ignore' });
        return true;
    } catch (_) {
        return false;
    }
}

console.log('\n  webprobe-cli: running post-install setup…\n');

if (!hasCargo()) {
    console.warn(
        '  ⚠  cargo (Rust) is not installed or not on PATH.\n' +
        '  webprobe is a Rust binary. Install Rust from https://rustup.rs then run:\n\n' +
        '     cargo install --git https://github.com/KaiavN/WebProbe.git\n\n' +
        '  Once installed, the `webprobe` command will be available.\n'
    );
    // Exit 0 so npm install doesn't fail
    process.exit(0);
}

console.log('  → Installing webprobe binary via cargo (this takes ~30s on first run)…\n');

const result = spawnSync(
    'cargo',
    ['install', '--git', 'https://github.com/KaiavN/WebProbe.git', '--force'],
    { stdio: 'inherit', shell: os.platform() === 'win32' }
);

if (result.status !== 0) {
    console.error(
        '\n  ✗  cargo install failed (exit code ' + result.status + ').\n' +
        '  Try running manually:\n' +
        '     cargo install --git https://github.com/KaiavN/WebProbe.git\n'
    );
    // Exit 0 so the npm package itself is still installed
    process.exit(0);
}

console.log('\n  ✓  webprobe installed successfully!\n');
console.log('  Run: webprobe --help\n');
