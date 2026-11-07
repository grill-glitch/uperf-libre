// Entry point. Holds the shared state, wires the four pages and the refresh loop.

import '@material/web/all.js';
import { toast } from 'kernelsu-alt';

import { icon } from './icons.js';
import { applyStaticTranslations, getString, loadTranslations } from './language.js';
import { setupRoute, currentTab } from './route.js';
import { readAll, readConfig, restart } from './ctl.js';
import * as home from './pages/home.js';
import * as mode from './pages/mode.js';
import * as dfps from './pages/dfps.js';
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
        state.configError = '';
    } catch (err) {
        // Keep the previously parsed config: a transient read failure should not empty the
        // preset list. When there has never been one, `mode.js` says so — but record the
        // reason so "stale config" is distinguishable from "no config".
        state.configError = err instanceof Error ? err.message : String(err);
        console.warn('config read failed', err);
    }
}

/** Every page redraws from the same snapshot, and every write ends in a reload. */
async function reload() {
    await load();
    await render();
}

async function render() {
    // Isolate the pages: one throwing renderer used to abort the rest and propagate out of
    // `reload()`, leaving a half-updated UI plus an unhandled rejection.
    const pages = [
        ['home', () => home.render(state.data, state.config, state.error, reload)],
        ['mode', () => mode.render(state.data, state.config, reload)],
        ['dfps', () => dfps.render(state.data, state.config, reload)],
        // `more.js` owns the confirmation dialog; it only needs the action itself.
        ['more', () => more.render(state.data, doRestart)],
    ];
    for (const [name, draw] of pages) {
        try {
            draw();
        } catch (err) {
            console.error(`render ${name} failed`, err);
        }
    }
}

window.__rerender = () => {
    applyStaticTranslations();
    // `render()` may return a rejected promise from an async page; swallow it here so callers
    // in other modules do not produce unhandled rejections.
    Promise.resolve(render()).catch((err) => console.error('render failed', err));
};

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
        toast(getString('toast_restart_fail', { err: err instanceof Error ? err.message : String(err) }));
    }
    await reload();
}

/** Startup sequence, separated so the splash teardown can live in a `finally`. */
async function startApp() {
    try {
        await loadTranslations();
    } catch {
        // Falling back to the key names is ugly but beats an empty page.
    }
    applyStaticTranslations();
    more.setupLogControls();

    setupRoute(async (tab) => {
        if (tab !== 'more') return;
        try {
            await more.refreshLog();
        } catch (err) {
            window.__toast?.(err instanceof Error ? err.message : String(err));
        }
    });

    const refreshBtn = document.getElementById('refresh-btn');
    if (refreshBtn) {
        refreshBtn.onclick = async () => {
            try {
                await reload();
                if (currentTab() === 'more') await more.refreshLog();
            } catch (err) {
                window.__toast?.(err instanceof Error ? err.message : String(err));
            }
        };
    }

    await reload();
}

// Only the chrome lives here; each page draws its own icons in its own module.
const STATIC_ICONS = {
    'splash-mark': 'bolt',
    'back-icon': 'arrow_back',
    'refresh-icon': 'refresh',
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

    // Everything below can throw (a missing node, a rejected read). Without the `finally`,
    // the splash overlay stayed on screen forever and the app looked hung.
    try {
        await startApp();
    } catch (err) {
        console.error('startup failed', err);
        window.__toast?.(err instanceof Error ? err.message : String(err));
    } finally {
        if (splash) {
            setTimeout(() => splash.classList.add('exit'), 50);
            setTimeout(() => splash.remove(), 400);
        }
    }

});
