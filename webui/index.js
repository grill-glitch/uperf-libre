// Entry point. Holds the shared state, wires the three pages and the refresh loop.

import '@material/web/all.js';
import { toast } from 'kernelsu-alt';

import { icon } from './icons.js';
import { applyStaticTranslations, getString, loadTranslations } from './language.js';
import { setupRoute, currentTab } from './route.js';
import { readAll, readConfig, restart } from './ctl.js';
import * as home from './pages/home.js';
import * as mode from './pages/mode.js';
import * as more from './pages/more.js';

// The page modules build their own markup (they render lists), so the two helpers they
// need are handed over explicitly rather than through a circular import.
window.__icon = icon;
window.__toast = (message) => toast(message);

const state = { data: {}, config: null, error: null };

async function load() {
    state.error = null;
    try {
        state.data = await readAll();
    } catch (err) {
        state.data = {};
        state.error = err.message;
    }
    try {
        state.config = await readConfig();
    } catch {
        // Keep the previously parsed config: a transient read failure should not empty
        // the preset list. When there has never been one, `mode.js` says so.
    }
}

async function render() {
    home.render(state.data, state.config, state.error);
    mode.render(state.data, state.config, async () => {
        await load();
        await render();
    });
    more.render(state.data, () => confirmRestart());
}

window.__rerender = () => {
    applyStaticTranslations();
    render();
};

function confirmRestart() {
    const dialog = document.getElementById('confirm-dialog');
    document.getElementById('confirm-title').textContent = getString('confirm_restart_title');
    document.getElementById('confirm-body').textContent = getString('confirm_restart_body');
    document.getElementById('confirm-cancel').onclick = () => dialog.close();
    document.getElementById('confirm-ok').onclick = async () => {
        dialog.close();
        await doRestart();
    };
    dialog.show();
}

async function doRestart() {
    try {
        const result = await restart();
        // `restart.ok` is the script's own verdict — the pid count *after* starting —
        // not an assumption that the command was accepted.
        if (result['restart.ok'] !== '1') {
            throw new Error(`daemon.count=${result['restart.after'] ?? '?'}`);
        }
        toast(getString('toast_restart_ok'));
    } catch (err) {
        toast(getString('toast_restart_fail', { err: err.message }));
    }
    await load();
    await render();
}

const STATIC_ICONS = {
    'splash-mark': 'bolt',
    'back-icon': 'arrow_back',
    'refresh-icon': 'refresh',
    'status-icon': 'speed',
    'cfg-icon': 'memory',
    'mode-icon': 'tune',
    'restart-icon': 'restart_alt',
    'language-icon': 'language',
    'log-refresh-icon': 'refresh',
};

document.addEventListener('DOMContentLoaded', async () => {
    document.querySelectorAll('[unresolved]').forEach((el) => el.removeAttribute('unresolved'));

    for (const [id, name] of Object.entries(STATIC_ICONS)) {
        const el = document.getElementById(id);
        if (el) el.innerHTML = icon(name);
    }
    for (const el of document.querySelectorAll('[data-icon]')) {
        el.innerHTML = icon(el.dataset.icon);
    }

    const splash = document.getElementById('splash');
    setTimeout(() => document.getElementById('splash-mark')?.classList.add('show'), 20);

    try {
        await loadTranslations();
    } catch {
        // Falling back to the key names is ugly but beats an empty page.
    }
    applyStaticTranslations();
    more.setupLogControls();

    setupRoute(async (tab) => {
        if (tab === 'more') await more.refreshLog();
    });

    document.getElementById('refresh-btn').onclick = async () => {
        await load();
        await render();
        if (currentTab() === 'more') await more.refreshLog();
    };

    await load();
    await render();

    if (splash) {
        setTimeout(() => splash.classList.add('exit'), 50);
        setTimeout(() => splash.remove(), 400);
    }
});
