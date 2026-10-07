// Tab 3 — the log, the project links, the language and a daemon restart.

import { getString, availableLanguages, currentLanguage, loadTranslations, applyStaticTranslations } from '../language.js';
import { MODULE_DIR, USER_PATH, readLog, restart, openUrl } from '../ctl.js';

const LOG_STEPS = [100, 500, 2000];
let logLines = 100;

const REPO = 'https://github.com/grill-glitch/uperf-libre';
const UPSTREAM = 'https://github.com/yc9559/uperf';
const DFPS = 'https://github.com/yc9559/dfps';

/** The android.os.Build-like facts belong to the OS, not to the module. */
export function render(data, onRestart) {
    const about = document.getElementById('about-card');
    about.textContent = '';
    addLink(about, 'layers', getString('label_platform'), 'dfps (yc9559) — Apache-2.0', DFPS);
    addLink(about, 'open_in_new', getString('label_upstream'), 'uperf (yc9559)', UPSTREAM);
    addLink(about, 'open_in_new', getString('label_source_repo'), 'uperf-libre', REPO);

    const license = document.createElement('div');
    license.className = 'row';
    const text = document.createElement('div');
    text.className = 'row-text';
    const title = document.createElement('div');
    title.className = 'row-title';
    title.textContent = `${getString('label_license')}: Apache-2.0`;
    const note = document.createElement('div');
    note.className = 'row-detail';
    note.textContent = getString('attr_text');
    text.append(title, note);
    license.append(text);
    about.append(license);

    document.getElementById('current-language').textContent = currentLanguage();

    document.getElementById('restart-item').onclick = () => onRestart?.();
    document.getElementById('language-item').onclick = () => cycleLanguage();
}

function addLink(container, iconName, label, value, url) {
    const row = document.createElement('div');
    row.className = 'row';
    const glyph = document.createElement('md-icon');
    glyph.className = 'row-icon';
    glyph.innerHTML = window.__icon(iconName);
    row.append(glyph);
    const text = document.createElement('div');
    text.className = 'row-text';
    const title = document.createElement('div');
    title.className = 'row-title';
    title.textContent = label;
    const detail = document.createElement('div');
    detail.className = 'row-detail';
    detail.textContent = value;
    text.append(title, detail);
    row.append(text);
    row.addEventListener('click', async () => {
        const ok = await openUrl(url);
        if (!ok) window.__toast(getString('msg_open_fail'));
    });
    container.append(row);
}

/** Two locales, so a tap toggles; a longer list would need a picker dialog. */
async function cycleLanguage() {
    const list = availableLanguages();
    const next = list[(list.indexOf(currentLanguage()) + 1) % list.length];
    await loadTranslations(next);
    applyStaticTranslations();
    window.__rerender();
    window.__toast(next);
}

export async function refreshLog() {
    const view = document.getElementById('log-view');
    view.textContent = getString('msg_loading');
    try {
        const text = await readLog(logLines);
        view.textContent = text.trim() ? text : getString('msg_no_log');
    } catch (err) {
        view.textContent = err.message;
    }
}

export function setupLogControls() {
    document.getElementById('log-refresh').onclick = () => refreshLog();
    for (const button of document.querySelectorAll('.log-lines md-text-button')) {
        button.addEventListener('click', () => {
            logLines = Number(button.dataset.lines) || LOG_STEPS[0];
            markActiveLines();
            refreshLog();
        });
    }
    markActiveLines();
}

function markActiveLines() {
    for (const button of document.querySelectorAll('.log-lines md-text-button')) {
        button.toggleAttribute('active', Number(button.dataset.lines) === logLines);
    }
}

/** Paths the about card documents, exported for the tests to assert against. */
export const paths = { MODULE_DIR, USER_PATH };
