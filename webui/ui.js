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
    const el = document.createElement('md-icon');
    el.className = `glyph${size === 24 ? '' : ` s${size}`}${extra ? ` ${extra}` : ''}`;
    el.innerHTML = window.__icon(name);
    return el;
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
    el.className = `glyph${size === 24 ? '' : ` s${size}`}${extra ? ` ${extra}` : ''}`;
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
    const dialog = document.getElementById('confirm-dialog');
    document.getElementById('confirm-title').textContent = getString(titleKey, params);
    document.getElementById('confirm-body').textContent = getString(bodyKey, params);
    document.getElementById('confirm-cancel').onclick = () => dialog.close();
    document.getElementById('confirm-ok').onclick = () => {
        dialog.close();
        onOk?.();
    };
    showDialog('confirm-dialog');
}

function showDialog(id) {
    const dialog = document.getElementById(id);
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
