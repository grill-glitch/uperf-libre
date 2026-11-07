// Generates `icons.js` from the `@material-symbols/svg-400` package.
//
// The paths are never typed by hand: they are read out of the published icon set, so
// every glyph is the authentic Material Symbols Outlined one. `npm run build` runs
// this first, which also means a missing/renamed icon fails the build instead of
// silently rendering nothing — in both directions:
//
//   * a name listed below that the package does not have  -> build fails (step 1);
//   * a name a call site uses that is not listed below    -> build fails (step 2).
//
// Step 2 was missing until a device run: the dfps tab called
// `icon('all_inclusive')`, which nobody had listed, so the build passed and the
// page threw `unknown icon: all_inclusive` on its first rule row — after it had
// already painted the header, so it read as an empty list rather than an error.
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, extname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');

// icon -> where it is used. Keep the comment: it is what makes a stale entry obvious.
const ICONS = {
    home: 'bottom bar: 首页',
    tune: 'bottom bar: 模式切换 / home: armed clusters stat',
    more_horiz: 'bottom bar: 更多',
    refresh: 'top bar refresh button / log refresh',
    speed: 'home: hero status card / bottom bar: 刷新率',
    devices: 'home: the device/module disclosure and its rows',
    bolt: 'splash mark / home: daemon pid stat',
    check_circle: 'mode: current preset',
    radio_button_unchecked: 'mode: selectable preset',
    restart_alt: 'more: restart daemon',
    info: 'more: about / the per-section explainer dialogs',
    open_in_new: 'about: external links',
    error: 'error card',
    warning: 'warning state',
    arrow_back: 'top bar: exit the WebUI',
    layers: 'about: platform layer',
    language: 'more: language row',
    all_inclusive: 'dfps: the universal "*" rule row',
    bedtime: 'dfps: the offscreen "-" rule row',
    apps: 'dfps: a per-app rule row',
    keyboard_arrow_down: 'collapsed disclosure',
    keyboard_arrow_up: 'expanded disclosure',
    edit: 'dfps: edit a rule',
    add: 'dfps: add a rule',
    content_copy: 'log: copy',
    download: 'log: export',
    fullscreen: 'log: enter full screen',
    fullscreen_exit: 'log: leave full screen',
    wrap_text: 'log: wrap long lines',
};

// --- 1. every listed icon must exist in the package -------------------------

const out = new Map();
const missing = [];
for (const name of Object.keys(ICONS)) {
    try {
        const svg = readFileSync(
            join(root, 'node_modules', '@material-symbols', 'svg-400', 'outlined', `${name}.svg`),
            'utf8',
        );
        const m = svg.match(/viewBox="([^"]+)"/);
        // Every path, and `d` not necessarily first: a glyph with two <path> children was
        // truncated to one, and an attribute before `d` was reported as an unexpected shape.
        const ds = [...svg.matchAll(/<path[^>]*\sd="([^"]+)"/g)].map((p) => p[1]);
        if (!m || !ds.length) throw new Error('unexpected svg shape');
        out.set(name, { viewBox: m[1], d: ds.join('') });
    } catch (e) {
        missing.push(`${name} (${e.message})`);
    }
}
if (missing.length) {
    console.error('gen-icons: missing icons:', missing.join(', '));
    process.exit(1);
}

// --- 2. every name a call site uses must be listed --------------------------
//
// Scans for `icon(...)` / `__icon(...)` / `glyph(...)` / `setGlyph(...)` and collects
// every lowercase string literal inside the call's parentheses. Parentheses are matched
// by depth and *all* literals are taken, because the call is often a ternary:
//
//     glyph(name === current ? 'check_circle' : 'radio_button_unchecked')
//
// which a regex anchored on `icon('` misses entirely — the same blind spot that let the
// original bug through. `ui.js` wraps the icon set behind `glyph()`/`setGlyph()`; those
// names are scanned too, so the wrapper cannot hide a name with no glyph behind it.
// `data-icon="..."` attributes are checked as well.
//
// Only literals are seen, so a name passed through a variable (the STATIC_ICONS maps)
// is not checked; those are reported as a warning instead.

const SCAN_EXT = new Set(['.js', '.mjs', '.html']);
const SCAN_CALLS = ['__icon', 'icon', 'glyph', 'setGlyph'];
// `tools/` holds the build-time generators, not UI code: they contain `icon(`
// inside comments and inside the `icons.js` template they emit, whose string
// literals ('node_modules', 'utf8', …) would otherwise be reported as unlisted
// icon names. Nothing under tools/ renders an icon.
const SKIP_DIRS = new Set(['node_modules', 'dist', 'build', '.git', 'tools']);
// Absolute path of the file this script writes: comparing basenames excluded *any* file named
// icons.js anywhere in the tree, letting its icon literals escape the unknown-icon check.
const GENERATED = join(root, 'icons.js');

function* sourceFiles(dir) {
    for (const entry of readdirSync(dir)) {
        if (SKIP_DIRS.has(entry)) continue;
        const full = join(dir, entry);
        if (statSync(full).isDirectory()) {
            yield* sourceFiles(full);
        } else if (SCAN_EXT.has(extname(entry)) && full !== GENERATED) {
            yield full;
        }
    }
}

/** Argument text of every `name( ... )` call, with parens matched by depth. */
function* callArgs(text, name) {
    const head = new RegExp(`\\b${name}\\s*\\(`, 'g');
    let m;
    while ((m = head.exec(text)) !== null) {
        let depth = 1;
        let i = m.index + m[0].length;
        const start = i;
        while (i < text.length && depth > 0) {
            if (text[i] === '(') depth += 1;
            else if (text[i] === ')') depth -= 1;
            i += 1;
        }
        yield text.slice(start, i - 1);
    }
}

/**
 * Everything before the first top-level comma — i.e. the icon-name argument.
 *
 * Only the name is checked. `glyph('edit', 18, 'dim')` passes CSS class names in the
 * later positions, and taking every literal in the call reported `dim`, `sel` and `chev`
 * as icons that are "used but not listed". Escaped quotes are not handled: none of these
 * call sites has one, and the only job here is finding the first top-level comma.
 */
function firstArg(args) {
    let depth = 0;
    let quote = null;
    for (let i = 0; i < args.length; i += 1) {
        const c = args[i];
        if (quote) {
            if (c === quote) quote = null;
            continue;
        }
        if (c === "'" || c === '"' || c === '`') { quote = c; continue; }
        if (c === '(' || c === '[' || c === '{') depth += 1;
        else if (c === ')' || c === ']' || c === '}') depth -= 1;
        else if (c === ',' && depth === 0) return args.slice(0, i);
    }
    return args;
}

const referenced = new Map(); // name -> Set(where)
const note = (name, where) => {
    if (!referenced.has(name)) referenced.set(name, new Set());
    referenced.get(name).add(where);
};

for (const file of sourceFiles(root)) {
    const text = readFileSync(file, 'utf8');
    const where = relative(root, file);
    for (const fn of SCAN_CALLS) {
        for (const args of callArgs(text, fn)) {
            // Accept single/double/backtick quotes: a `icon("home")` call was silently skipped
            // and never validated against ICONS.
            for (const [, lit] of firstArg(args).matchAll(/['"`]([a-z_0-9]+)['"`]/g)) note(lit, `${where}:${fn}`);
        }
    }
    for (const [, lit] of text.matchAll(/data-icon="([a-z_0-9]+)"/g)) note(lit, `${where}:data-icon`);
}

const unlisted = [...referenced.keys()].filter((n) => !Object.hasOwn(ICONS, n)).sort();
if (unlisted.length) {
    console.error('gen-icons: icons used but not listed in ICONS:');
    for (const n of unlisted) console.error(`  ${n}  <- ${[...referenced.get(n)].join(', ')}`);
    console.error('  add them to ICONS (with the usage comment) and re-run');
    process.exit(1);
}

const unused = Object.keys(ICONS).filter((n) => !referenced.has(n));
if (unused.length) {
    console.warn(`gen-icons: listed but only referenced via a variable: ${unused.join(', ')}`);
}

// --- 3. emit ----------------------------------------------------------------

const body = [...out.entries()]
    .map(([name, { viewBox, d }]) => `    ${JSON.stringify(name)}: { viewBox: ${JSON.stringify(viewBox)}, d: ${JSON.stringify(d)} },`)
    .join('\n');

writeFileSync(
    join(root, 'icons.js'),
    `// GENERATED by tools/gen-icons.mjs from @material-symbols/svg-400 — do not edit.\n` +
    `// ${out.size} Material Symbols Outlined glyphs.\n` +
    `export const ICONS = {\n${body}\n};\n\n` +
    `/** An inline <svg> string for \`name\`, sized by the surrounding CSS. */\n` +
    `export function icon(name) {\n` +
    `    const g = Object.hasOwn(ICONS, name) ? ICONS[name] : null;\n` +
    `    if (!g) throw new Error('unknown icon: ' + name);\n` +
    `    return \`<svg xmlns="http://www.w3.org/2000/svg" viewBox="\${g.viewBox}" fill="currentColor"><path d="\${g.d}"/></svg>\`;\n` +
    `}\n`,
);
console.log(`gen-icons: wrote icons.js with ${out.size} icons`);
