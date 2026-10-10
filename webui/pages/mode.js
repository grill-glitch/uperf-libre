// Tab 2 — the mode switch: writes `cur_powermode.txt`.
//
// One list, one selection. The previous version drew the current preset three times
// over: a card at the top, a row in the list, and a pinned card at the bottom carrying
// the same sentence as the row — with "defined by the loaded config" repeated under
// every entry. Here the current preset is simply the checked row, the explanatory text
// lives behind the ⓘ, and nothing is said twice.
//
// `auto` is presented as a row of the same list because it is one of the choices, not a
// separate concept: it hands switching to the per-app rules. Both the list and the home
// page's chips come from `presets.js`, so the two can never offer different sets.

import { getString } from '../language.js';
import { glyph, setGlyph, openInfo } from '../ui.js';
import { USER_PATH } from '../ctl.js';
import { AUTO, presetLabel, presetNames, isDefinedPreset, applyPreset } from '../presets.js';

/** Set by `render`; the row click needs the app's reload-and-redraw callback. */
let onChanged = null;

export function render(data, config, onPresetChanged) {
    const current = data['preset.present'] === '1' ? data['preset.current'] : '';
    onChanged = onPresetChanged;

    setGlyph('info', document.getElementById('mode-info-icon'), 16);
    document.getElementById('mode-info').onclick = () => openInfo('mode_info_title', [
        { text: 'mode_info_what' },
        { text: 'mode_hint_auto' },
        { text: 'mode_info_preset' },
        { text: 'mode_info_file', params: { path: `${USER_PATH}/cur_powermode.txt` } },
    ]);

    const list = document.getElementById('preset-list');
    list.textContent = '';

    // A value the config does not define can still be in the file (a config was swapped
    // under it). Showing it as the checked row is honest; hiding it would leave the page
    // claiming no mode is set while the daemon is running one.
    if (current && !presetNames(config).includes(current)) {
        list.append(modeRow(current, current, undefined));
    }
    for (const name of presetNames(config)) {
        list.append(modeRow(name, current, config));
    }

    document.getElementById('mode-note').textContent = config
        ? getString('mode_hint_inode', { path: `${USER_PATH}/cur_powermode.txt` })
        : getString('msg_no_config', { path: `${USER_PATH}/uperf.json` });
}

function modeRow(name, current, config) {
    const row = document.createElement('div');
    row.className = 'preset';
    row.setAttribute('role', 'radio');
    row.setAttribute('aria-current', String(name === current));
    row.dataset.preset = name;
    row.append(document.createElement('md-ripple'));
    row.append(glyph(name === current ? 'check_circle' : 'radio_button_unchecked', 22, 'sel'));

    const text = document.createElement('div');
    text.className = 'preset-text';
    const title = document.createElement('div');
    title.className = 'preset-title';
    title.textContent = presetLabel(name);
    text.append(title);

    // Only `auto` needs a line of its own, and only when the config is loaded: the
    // generic "defined by the loaded config" subtitle said nothing about any row.
    if (config === undefined) {
        text.append(subLine('preset_undefined'));
    } else if (name === AUTO) {
        text.append(subLine('mode_hint_auto'));
    } else if (config && !isDefinedPreset(config, name)) {
        text.append(subLine('preset_undefined'));
    }
    row.append(text);
    row.onclick = () => applyPreset(name, row, onChanged);
    return row;
}

function subLine(key) {
    const el = document.createElement('div');
    el.className = 'preset-sub';
    el.textContent = getString(key);
    return el;
}
