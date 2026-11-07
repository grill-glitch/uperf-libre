// Small DOM helpers shared by the pages.
//
// Before this file each page built its own `<div class="row">` trees and they had
// drifted apart (the log card and the supervision card both had a private copy of the
// same three lines). Everything that more than one page draws lives here.
//
// On icons: `glyph()` and `setGlyph()` are scanned by `tools/gen-icons.mjs` for their
// string literals, exactly like `icon()`/`__icon()`. That check is the only thing that
// catches a glyph name with no entry in the generated set — it must not be routed
// around by an indirection.

import { getString } from './language.js';

/** `<md-icon>` carrying a generated Material Symbols glyph. */
export function glyph(name, size = 24, extra = '') {
    // Delegates: the class/innerHTML construction used to exist twice and could drift.
    return setGlyph(name, document.createElement('md-icon'), size, extra);
}

/**
 * Same, into an element that already exists in `index.html`.
 *
 * The icon name is the **first** argument on purpose: `tools/gen-icons.mjs` only checks
 * the first argument of these calls, so `setGlyph('devices', el)` is verified while
 * `setGlyph(el, 'devices')` would slip through unchecked.
 */
export function setGlyph(name, el, size = 24, extra = '') {
    if (!el) return el;
    // classList, not `className =`: the old form dropped any class the markup already put on
    // the element (e.g. `<md-icon class="glyph s16" id="…">`).
    el.classList.add('glyph');
    el.classList.toggle(`s${size}`, size !== 24);
    if (extra) {
        for (const cls of extra.split(' ')) {
            if (cls) el.classList.add(cls);
        }
    }
    if (typeof window.__icon !== 'function') {
        throw new Error('window.__icon is not installed (index.js not evaluated?)');
    }
    el.innerHTML = window.__icon(name);
    return el;
}

/**
 * Fills `container` with label/value rows in a two-column grid.
 *
 * `pairs` is `[label, value, opts]`; a row whose value is empty is skipped rather than
 * rendered as a label with nothing after it. `opts.mono` marks reference material
 * (hashes, paths, versions) so it renders visibly secondary to the values above it.
 */
export function kvRows(container, pairs) {
    container.textContent = '';
    for (const [label, value, opts] of pairs) {
        if (value === undefined || value === null || value === '') continue;
        const l = document.createElement('div');
        l.className = 'kv-label';
        l.textContent = label;
        const v = document.createElement('div');
        v.className = `kv-value${opts?.mono ? ' mono' : ''}`;
        v.textContent = value;
        container.append(l, v);
    }
    if (!container.childElementCount) {
        const empty = document.createElement('div');
        empty.className = 'empty';
        empty.textContent = getString('msg_no_data');
        container.append(empty);
    }
}

/**
 * One explainer dialog for the whole page.
 *
 * `blocks` is a list of `{ text, params }` paragraphs or `{ list: [key, …] }` bullet
 * lists. The content is built from locale keys, so nothing here is a hardcoded string
 * and both locales stay in step.
 */
export function openInfo(titleKey, blocks, params) {
    const dialog = document.getElementById('info-dialog');
    document.getElementById('info-title').textContent = getString(titleKey, params);
    const body = document.getElementById('info-body');
    body.textContent = '';
    for (const block of blocks) {
        if (block.list) {
            const ul = document.createElement('ul');
            for (const key of block.list) {
                const li = document.createElement('li');
                li.textContent = getString(key, params);
                ul.append(li);
            }
            body.append(ul);
            continue;
        }
        const p = document.createElement('p');
        p.textContent = getString(block.text, { ...params, ...block.params });
        body.append(p);
    }
    // The Close button is the dialog's only exit: without this handler the click did
    // nothing and the overlay sat there (the same wiring `askConfirm` does per call, not
    // once at startup, so a page that never opens the dialog cannot break it).
    document.getElementById('info-ok').onclick = () => dialog.close();
    showDialog('info-dialog');
}

/** Confirmation dialog. `onOk` runs after the dialog closes. */
export function askConfirm(titleKey, bodyKey, params, onOk) {
    // Resolve every node once, with a message that names the missing id: an unattributed
    // `Cannot set properties of null` was the old failure mode.
    const dialog = document.getElementById('confirm-dialog');
    const title = document.getElementById('confirm-title');
    const body = document.getElementById('confirm-body');
    const cancel = document.getElementById('confirm-cancel');
    const ok = document.getElementById('confirm-ok');
    if (!dialog || !title || !body || !cancel || !ok) {
        throw new Error('confirm dialog markup is incomplete');
    }
    title.textContent = getString(titleKey, params);
    body.textContent = getString(bodyKey, params);
    cancel.onclick = () => dialog.close();
    ok.onclick = async () => {
        dialog.close();
        // `onOk` may be async (the restart path is): a rejection used to become an unhandled
        // promise rejection inside a DOM handler, with no feedback at all.
        try {
            await onOk?.();
        } catch (err) {
            window.__toast?.(err instanceof Error ? err.message : String(err));
        }
    };
    dialog.show();
}

function showDialog(id) {
    const dialog = document.getElementById(id);
    if (!dialog) throw new Error(`dialog #${id} is missing`);
    dialog.show();
    return dialog;
}

/** Marks `el` busy for the duration of `fn`; a second tap cannot start a second write. */
export async function withBusy(el, fn) {
    el?.classList.add('busy');
    try {
        return await fn();
    } finally {
        el?.classList.remove('busy');
    }
}
