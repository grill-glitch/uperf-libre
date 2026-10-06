// Tab 4 — dfps: shows the current Hz, lists dfps.txt rules, and lets the
// user set/edit one rule at a time. Mirrors mode.js's shape: render(data)
// reads what `dfps.sh status|info` printed; apply() calls
// `dfps.sh set-rule <pkg> <idle> <active>` and re-reads on success.

import { getString } from '../language.js';
import { setRule, dfpsStatus, dfpsInfo } from '../ctl.js';

let onChanged = null;

export async function render(_data, _config, onRefresh) {
    onChanged = onRefresh;
    await refresh();
}

async function refresh() {
    let status, info;
    try {
        status = await dfpsStatus();
        info = await dfpsInfo();
    } catch (e) {
        document.getElementById('dfps-cur').textContent = '—';
        document.getElementById('dfps-rules').textContent = '';
        return;
    }
    document.getElementById('dfps-cur').textContent = status['cur'] || '—';
    document.getElementById('dfps-config-path').textContent = status['config.path'] || '';
    const list = document.getElementById('dfps-rules');
    list.textContent = '';
    const raw = (info['rules'] || '').split('|').filter(Boolean);
    for (const line of raw) {
        const parts = line.trim().split(/\s+/);
        if (parts.length !== 3) continue;
        const [pkg, idle, active] = parts;
        try {
            list.append(ruleRow(pkg, parseInt(idle, 10), parseInt(active, 10)));
        } catch (err) {
            // One row that cannot be built must not blank the whole list. That is
            // exactly how a missing icon presented: `icon()` throws on an
            // unlisted name, the throw escaped this loop, and the page showed a
            // painted header over an empty list — indistinguishable from "no
            // rules". Degrade to a missing row and keep the rest.
            console.error(`dfps: cannot render rule row "${line}"`, err);
        }
    }
}

function ruleRow(pkg, idle, active) {
    const row = document.createElement('div');
    row.className = 'preset';
    row.dataset.pkg = pkg;

    const glyph = document.createElement('md-icon');
    glyph.innerHTML = window.__icon(pkg === '*' ? 'all_inclusive'
        : pkg === '-' ? 'bedtime'
        : 'apps');
    row.append(glyph);

    const text = document.createElement('div');
    text.className = 'card-text';
    const title = document.createElement('div');
    title.className = 'card-title';
    title.textContent = pkg;
    const detail = document.createElement('div');
    detail.className = 'card-subtitle';
    detail.textContent = `${getString('dfps_idle')}: ${idle}  ${getString('dfps_active')}: ${active}`;
    text.append(title, detail);
    row.append(text);

    row.addEventListener('click', () => edit(row, pkg, idle, active));
    return row;
}

async function edit(row, pkg, idle, active) {
    const newIdle = prompt(getString('dfps_prompt_idle', { pkg, idle }), String(idle));
    if (newIdle === null) return;
    const newActive = prompt(getString('dfps_prompt_active', { pkg, active }), String(active));
    if (newActive === null) return;
    row.classList.add('busy');
    try {
        const result = await setRule(pkg, newIdle, newActive);
        if (result['rule.ok'] !== '1') {
            throw new Error(result['rule.error'] || 'write failed');
        }
        window.__toast(getString('toast_rule_ok', { pkg }));
        await onChanged?.();
    } catch (e) {
        window.__toast(getString('toast_rule_fail', { err: e.message }));
    } finally {
        row.classList.remove('busy');
        await refresh();
    }
}