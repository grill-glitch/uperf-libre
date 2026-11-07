// The only place that talks to the device.
//
// Everything goes through the module's own `script/webui.sh`, which is also what the
// shell tests exercise, so the WebUI and the adb-verified contract cannot drift. Each
// call returns the script's `key=value` output parsed into an object — the frontend
// never parses prose.

import { exec } from 'kernelsu-alt';

export const MODULE_DIR = '/data/adb/modules/uperf';
export const USER_PATH = '/sdcard/Android/yc/uperf';
const SCRIPT = `${MODULE_DIR}/script/webui.sh`;
const DFPS_SCRIPT = `${MODULE_DIR}/script/dfps.sh`;

/** POSIX shell single-quote escaping; preset names come from a text file. */
export function shellQuote(value) {
    return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

export function parseKeyValue(text) {
    const out = {};
    for (const line of String(text).split('\n')) {
        const eq = line.indexOf('=');
        if (eq > 0) out[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
    }
    return out;
}

/**
 * Runs the control script. The subcommand is **not** quoted, so the command string is
 * byte-identical to the one the shell tests use (`sh <script> set-preset 'balance'`);
 * quoting it as well worked, but it made the shipped command differ from the verified
 * one, which is the kind of gap that hides a real mismatch later.
 */
async function runScript(script, subcommand, ...args) {
    // One runner for both device scripts: the two copies were byte-identical apart from the
    // path, so any quoting/timeout fix had to be made twice (and silently diverged).
    const parts = [`sh ${script}`, subcommand, ...args.map(shellQuote)];
    const result = await exec(parts.join(' '));
    const err = (result.stderr || '').trim();
    // The script always prints its verdict on stdout, so a non-zero exit without any output
    // is the case that means "the shell bridge itself failed". Keep the stderr either way: a
    // partial result followed by a failure must not look like a clean run.
    if (result.errno !== 0 && !result.stdout) {
        throw new Error(err || `exit ${result.errno}`);
    }
    return { ...result, stderr: err };
}

async function ctl(subcommand, ...args) {
    return runScript(SCRIPT, subcommand, ...args);
}

export async function readInfo() {
    return parseKeyValue((await ctl('info')).stdout);
}

export async function readStatus() {
    return parseKeyValue((await ctl('status')).stdout);
}

/** Device facts and runtime state in one round trip. */
export async function readAll() {
    return parseKeyValue((await ctl('all')).stdout);
}

/**
 * The config the daemon loaded, parsed in JS.
 *
 * The preset names the mode page offers have to come from this file: the shipped
 * configs are not all alike (sdm865 defines `crazy` as a fifth preset), so a hardcoded
 * list would offer presets that do not exist and omit ones that do.
 */
export async function readConfig() {
    const result = await exec(`cat ${shellQuote(`${USER_PATH}/uperf.json`)}`);
    if (result.errno !== 0) {
        throw new Error((result.stderr || '').trim() || `exit ${result.errno}`);
    }
    // A truncated/invalid file surfaced as a raw `SyntaxError` from deep inside the bridge
    // call, and a `null`/array body then blew up at the first property access.
    let config;
    try {
        config = JSON.parse(result.stdout);
    } catch (e) {
        throw new Error(`cannot parse ${USER_PATH}/uperf.json: ${e.message}`);
    }
    if (!config || typeof config !== 'object' || Array.isArray(config)) {
        throw new Error(`${USER_PATH}/uperf.json is not a JSON object`);
    }
    return config;
}

export async function setPreset(name) {
    return parseKeyValue((await ctl('set-preset', name)).stdout);
}

export async function readLog(lines) {
    // `undefined`/`NaN` used to reach the shell as `-n undefined`, which either fails
    // opaquely or silently returns the whole file.
    const n = Number(lines);
    if (!Number.isInteger(n) || n <= 0) throw new Error(`invalid line count: ${lines}`);
    return (await ctl('log', String(n))).stdout;
}

/**
 * Copies a slice of the log to `$USER_PATH/export/` and reports where it landed.
 *
 * The log is shown in a WebView `<pre>`, which on a phone cannot be selected and
 * copied out of; this is the only action that actually gets the text off the device.
 * The script re-reads the file it wrote, so `export.ok=1` means the bytes are there.
 */
export async function exportLog(lines) {
    return parseKeyValue((await ctl('export', String(lines))).stdout);
}

export async function restart() {
    return parseKeyValue((await ctl('restart')).stdout);
}

/** dfps control entry — same `key=value` protocol as webui.sh. */
async function dfpsCtl(subcommand, ...args) {
    return runScript(DFPS_SCRIPT, subcommand, ...args);
}

export async function dfpsStatus() {
    return parseKeyValue((await dfpsCtl('status')).stdout);
}

export async function dfpsInfo() {
    return parseKeyValue((await dfpsCtl('info')).stdout);
}

export async function setRule(pkg, idle, active) {
    return parseKeyValue((await dfpsCtl('set-rule', pkg, idle, active)).stdout);
}

/** Opens a URL through Android, with a WebView fallback. */
export async function openUrl(url) {
    // `exec` can reject (bridge unavailable, KSU permission error): that used to escape as an
    // opaque unhandled rejection. `am start` also exits 0 when nothing resolves the intent, so
    // the boolean only means "the command ran".
    try {
        const result = await exec(`am start -a android.intent.action.VIEW -d ${shellQuote(url)}`);
        return result.errno === 0;
    } catch (e) {
        throw new Error(`cannot open URL: ${e instanceof Error ? e.message : String(e)}`);
    }
}
