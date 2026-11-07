// Preset names and the preset write path.
//
// Shared by the home page's quick chips and the 模式切换 radio list, which must offer
// the same set and make the same write — the two differ only in how they look.
//
// The list comes from the **loaded config's** `presets` keys, not from a fixed four:
// `sdm865.json` defines a fifth preset (`crazy`), and a config could just as well drop
// one. Offering a name the config does not define would make the daemon log
// `Failed to switch to undefined preset '…'`.

import { getString } from './language.js';
import { setPreset } from './ctl.js';

const PRESET_KEYS = {
    powersave: 'preset_powersave',
    balance: 'preset_balance',
    performance: 'preset_performance',
    fast: 'preset_fast',
    crazy: 'preset_crazy',
};

/** Display order, weakest first. Names outside this list follow, sorted. */
const CANONICAL = ['powersave', 'balance', 'performance', 'fast', 'crazy'];

export const AUTO = 'auto';

/** Short label for a chip: `省电`, `自动`. Unknown names show as-is. */
export function shortPresetLabel(name) {
    if (name === AUTO) return getString('preset_auto');
    const key = PRESET_KEYS[name];
    return key ? getString(key) : name;
}

/** `省电 (powersave)` for a known preset, the bare name for anything else. */
export function presetLabel(name) {
    if (name === AUTO) return `${getString('preset_auto')} (${AUTO})`;
    const key = PRESET_KEYS[name];
    return key ? `${getString(key)} (${name})` : name;
}

/**
 * The names to offer, `auto` first.
 *
 * `auto` is a legal value of `cur_powermode.txt` but is not a preset name: it hands
 * switching to the per-app rules. It is a row of the same list rather than a separate
 * card, because it is one of the choices the user picks between.
 */
export function presetNames(config) {
    const defined = config && config.presets ? Object.keys(config.presets) : [];
    const known = CANONICAL.filter((n) => defined.includes(n));
    const rest = defined.filter((n) => !CANONICAL.includes(n)).sort();
    return [AUTO, ...known, ...rest];
}

/** One preset write at a time: a module-level guard, so a second tap is a no-op. */
let inFlight = false;

/** True when `name` is a preset the loaded config defines (so not `auto`). */
export function isDefinedPreset(config, name) {
    // `in` walks the prototype chain, so `toString`/`constructor`/`valueOf` looked like
    // presets the config defines.
    return Boolean(config && config.presets && Object.prototype.hasOwnProperty.call(config.presets, name));
}

/**
 * Performs the write and re-renders from what the file now contains.
 *
 * `onChanged` is what makes the UI honest: the script re-reads `cur_powermode.txt`
 * after writing, and the page redraws from that read rather than from the tap.
 */
export async function applyPreset(name, el, onChanged) {
    if (el?.getAttribute('aria-current') === 'true') return;
    // Real in-flight guard, not the cosmetic `busy` class: two rapid taps on different rows
    // used to run two writes concurrently (last writer wins, UI reads from a race).
    if (inFlight) return;
    inFlight = true;
    el?.classList.add('busy');
    try {
        const result = await setPreset(name);
        if (result['preset.ok'] !== '1') {
            throw new Error(result['preset.error'] || 'write failed');
        }
        // Success toast and refresh *outside* the write's error handling: a failing refresh
        // used to be reported as a failed write (the user saw both a success and a failure).
        window.__toast?.(getString('toast_preset_ok', { name: presetLabel(name) }));
    } catch (err) {
        window.__toast?.(getString('toast_preset_fail', { err: err instanceof Error ? err.message : String(err) }));
        return;
    } finally {
        inFlight = false;
        el?.classList.remove('busy');
    }
    try {
        await onChanged?.();
    } catch (err) {
        console.warn('reload after preset change failed', err);
    }
}
