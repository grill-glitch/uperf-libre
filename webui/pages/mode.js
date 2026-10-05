// Tab 2 — the mode switch: writes `cur_powermode.txt`.
//
// The list is built from the **loaded config's** `presets` keys plus `auto`, because
// the shipped configs differ: sdm865 defines a fifth preset (`crazy`) that the four
// canonical names would miss, and a config could just as well drop one. Offering a
// name the config does not define would make the daemon log
// `Failed to switch to undefined preset '…'`, so the page cannot offer it.
//
// `auto` is a legal value of the file but is *not* a preset name: it hands switching to
// the per-app rules. The hint says that much and no more.

import { getString } from '../language.js';
import { USER_PATH, setPreset } from '../ctl.js';

/** Names of the canonical presets get a translation; anything else shows as-is. */
const PRESET_KEYS = {
    powersave: 'preset_powersave',
    balance: 'preset_balance',
    performance: 'preset_performance',
    fast: 'preset_fast',
    crazy: 'preset_crazy',
};

export function presetLabel(name) {
    const key = PRESET_KEYS[name];
    return key ? `${getString(key)} (${name})` : name;
}

let onChanged = null;

export function render(data, config, onPresetChanged) {
    onChanged = onPresetChanged;
    const current = data['preset.present'] === '1' ? data['preset.current'] : '';
    document.getElementById('mode-current').textContent =
        current ? presetLabel(current) : getString('mode_current_none');
    document.getElementById('mode-current-hint').textContent = current === 'auto'
        ? getString('mode_hint_auto')
        : `${USER_PATH}/cur_powermode.txt`;

    const list = document.getElementById('preset-list');
    list.textContent = '';

    const names = config && config.presets ? Object.keys(config.presets) : [];
    for (const name of names) {
        list.append(presetRow(name, current, false));
    }
    list.append(presetRow('auto', current, true));

    document.getElementById('mode-note').textContent = names.length
        ? getString('mode_hint_preset')
        : getString('msg_no_config', { path: `${USER_PATH}/uperf.json` });
}

function presetRow(name, current, isAuto) {
    const row = document.createElement('div');
    row.className = 'preset';
    row.setAttribute('aria-current', String(name === current));
    row.dataset.preset = name;

    const glyph = document.createElement('md-icon');
    glyph.innerHTML = window.__icon(name === current ? 'check_circle' : 'radio_button_unchecked');
    row.append(glyph);

    const text = document.createElement('div');
    text.className = 'card-text';
    const title = document.createElement('div');
    title.className = 'card-title';
    title.textContent = isAuto ? `${getString('preset_auto')} (auto)` : presetLabel(name);
    const detail = document.createElement('div');
    detail.className = 'card-subtitle';
    detail.textContent = isAuto ? getString('mode_hint_auto') : getString('preset_defined');
    text.append(title, detail);
    row.append(text);

    row.addEventListener('click', () => apply(name, row));
    return row;
}

async function apply(name, row) {
    if (row.getAttribute('aria-current') === 'true') return;
    row.classList.add('busy');
    try {
        const result = await setPreset(name);
        // The script writes and then re-reads the file, so `preset.ok` is the file's
        // own content, not the intent.
        if (result['preset.ok'] !== '1') {
            throw new Error(result['preset.error'] || 'write failed');
        }
        window.__toast(getString('toast_preset_ok', { name: presetLabel(name) }));
        await onChanged?.();
    } catch (err) {
        window.__toast(getString('toast_preset_fail', { err: err.message }));
    } finally {
        row.classList.remove('busy');
    }
}
