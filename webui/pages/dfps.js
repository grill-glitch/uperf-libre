// Tab 3 — refresh-rate rules (dfps).
//
// Reads what `dfps.sh status|info` printed; writes through `dfps.sh set-rule`.
//
// The editor is a real dialog. The previous version used two chained `prompt()` boxes,
// which cannot show the old values side by side and are suppressed outright in some
// WebViews (returning null, so editing silently did nothing). The same dialog adds a
// rule — the contract always supported it, the page just had no way to ask.

import { getString } from '../language.js';
import { glyph, setGlyph } from '../ui.js';
import { setRule, dfpsStatus, dfpsInfo } from '../ctl.js';

let onChanged = null;
/** `{ pkg }` while the dialog edits an existing rule, `null` while it adds one. */
let editing = null;
let wired = false;

export async function render(_data, _config, onRefresh) {
    onChanged = onRefresh;
    setGlyph('add', document.getElementById('dfps-add-icon'), 16);
    document.getElementById('dfps-add').onclick = () => openEditor(null);
    setupDialog();
    await refresh();
}

async function refresh() {
    const list = document.getElementById('dfps-rules');
    let status;
    let info;
    try {
        // Independent shell round-trips: serially awaiting them doubled every refresh.
        [status, info] = await Promise.all([dfpsStatus(), dfpsInfo()]);
    } catch (e) {
        document.getElementById('dfps-cur').textContent = '—';
        document.getElementById('dfps-config-path').textContent = '';
        document.getElementById('dfps-rule-count').textContent = '';
        list.textContent = '';
        list.append(emptyRow(getString('dfps_no_status')));
        return;
    }

    // `dfps.sh status` prints `(none)`/`(empty)` when dfps_cur.txt is missing or empty: those
    // were rendered as "(none) Hz" instead of the placeholder.
    const cur = status['cur'];
    const hasCur = Boolean(cur) && cur !== '(none)' && cur !== '(empty)';
    document.getElementById('dfps-cur').textContent = hasCur ? `${cur} Hz` : '—';
    document.getElementById('dfps-config-path').textContent = status['config.path'] || '';

    // The script packs the rule file into `pkg idle active|pkg idle active|…`. A line
    // that does not parse is dropped rather than guessed at, and one bad row must not
    // blank the list — that is exactly how a missing icon once presented: the throw
    // escaped the loop and left a painted header over an empty list, which reads as
    // "no rules" rather than as a failure.
    const rules = [];
    for (const line of (info['rules'] || '').split('|').filter(Boolean)) {
        const parts = line.trim().split(/\s+/);
        if (parts.length !== 3) continue;
        const [pkg, idle, active] = parts;
        if (!/^\d+$/.test(idle) || !/^\d+$/.test(active)) continue;
        rules.push({ pkg, idle: Number(idle), active: Number(active) });
    }

    list.textContent = '';
    if (rules.length) {
        for (const rule of rules) list.append(ruleRow(rule));
    } else {
        list.append(emptyRow(getString('dfps_no_rules')));
    }
    document.getElementById('dfps-rule-count').textContent = String(rules.length);
}

/** Icon for a rule row: the two special tokens, else a normal app. */
function packageIcon(pkg) {
    if (pkg === '*') return 'all_inclusive';
    if (pkg === '-') return 'bedtime';
    return 'apps';
}

function packageLabel(pkg) {
    if (pkg === '*') return getString('dfps_any_app');
    if (pkg === '-') return getString('dfps_offscreen');
    return pkg;
}

function ruleRow(rule) {
    const row = document.createElement('div');
    row.className = 'rule';
    row.dataset.pkg = rule.pkg;
    row.setAttribute('role', 'button');
    row.append(document.createElement('md-ripple'));
    row.append(glyph(packageIcon(rule.pkg), 22, 'dim'));

    const text = document.createElement('div');
    text.className = 'rule-text';
    const pkg = document.createElement('div');
    pkg.className = 'rule-pkg';
    // The two tokens the file actually uses are shown next to the label: the rule file
    // is what the user edits, and a bare `*` on its own line read as a stray bullet.
    pkg.textContent = rule.pkg === '*' || rule.pkg === '-'
        ? `${packageLabel(rule.pkg)} (${rule.pkg})`
        : packageLabel(rule.pkg);
    text.append(pkg);
    row.append(text);

    const hz = document.createElement('div');
    hz.className = 'rule-hz';
    hz.append(hzItem(getString('dfps_idle'), rule.idle), hzItem(getString('dfps_active'), rule.active));
    row.append(hz);
    row.append(glyph('edit', 18, 'dim edit-hint'));

    row.onclick = () => openEditor(rule);
    return row;
}

function hzItem(label, value) {
    const el = document.createElement('div');
    el.className = 'rule-hz-item';
    const v = document.createElement('div');
    v.className = 'rule-hz-value';
    v.textContent = String(value);
    const l = document.createElement('div');
    l.className = 'rule-hz-label';
    l.textContent = label;
    el.append(v, l);
    return el;
}

function emptyRow(text) {
    const el = document.createElement('div');
    el.className = 'empty';
    el.textContent = text;
    return el;
}

function setupDialog() {
    if (wired) return;
    wired = true;
    const dialog = document.getElementById('field-dialog');
    document.getElementById('field-cancel').onclick = () => dialog.close();
    document.getElementById('field-save').onclick = () => save();
}

function openEditor(rule) {
    editing = rule ? { pkg: rule.pkg } : null;
    const dialog = document.getElementById('field-dialog');
    document.getElementById('field-title').textContent = rule
        ? getString('dfps_edit_title', { pkg: packageLabel(rule.pkg) })
        : getString('dfps_add_title');
    document.getElementById('field-pkg-wrap').hidden = Boolean(rule);
    document.getElementById('field-pkg').value = rule ? rule.pkg : '';
    document.getElementById('field-idle').value = rule ? String(rule.idle) : '';
    document.getElementById('field-active').value = rule ? String(rule.active) : '';
    document.getElementById('field-note').textContent = getString(rule ? 'dfps_note_edit' : 'dfps_note_add');
    dialog.show();
}

async function save() {
    const dialog = document.getElementById('field-dialog');
    const pkg = editing ? editing.pkg : document.getElementById('field-pkg').value.trim();
    const idle = document.getElementById('field-idle').value.trim();
    const active = document.getElementById('field-active').value.trim();

    // dfps.sh writes the value verbatim as `pkg idle active` and also uses it in a grep
    // pattern: whitespace splits the line, a pipe breaks the `info` encoding, and regex
    // metacharacters match the wrong rule. Only a real package name or the two special
    // tokens may pass.
    if (!/^(?:[A-Za-z0-9._:]+|\*|-)$/.test(pkg)) {
        window.__toast(getString('dfps_err_pkg'));
        return;
    }
    if (!isHz(idle) || !isHz(active)) {
        window.__toast(getString('dfps_err_hz'));
        return;
    }

    const button = document.getElementById('field-save');
    button.disabled = true;
    try {
        const result = await setRule(pkg, idle, active);
        if (result['rule.ok'] !== '1') {
            throw new Error(result['rule.error'] || 'write failed');
        }
        window.__toast(getString(editing ? 'toast_rule_ok' : 'toast_rule_added', { pkg }));
        dialog.close();
        await onChanged?.();
    } catch (err) {
        window.__toast(getString('toast_rule_fail', { err: err.message }));
    } finally {
        button.disabled = false;
    }
}

/** A refresh rate in Hz. The script would take anything; the page should not offer to. */
function isHz(value) {
    return /^\d{1,4}$/.test(value) && Number(value) > 0;
}
