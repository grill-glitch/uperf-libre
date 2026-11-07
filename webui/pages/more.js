// Tab 4 — the log (with the actions a phone actually needs), the project links, the
// language and a daemon restart.
//
// The log viewer is the part of the page that was genuinely unusable: a 46dvh box of
// 11px monospace with no wrapping, so every one of the daemon's ~150-column sched lines
// was cut off, and no way to get the text out of the WebView at all. So: wrapping is the
// default, one tap opens it full screen, one copies, and one writes a file the user can
// actually reach (see `webui.sh export`).

import { getString, availableLanguages, currentLanguage, loadTranslations, applyStaticTranslations } from '../language.js';
import { MODULE_DIR, USER_PATH, readLog, exportLog, openUrl } from '../ctl.js';
import { glyph, setGlyph, askConfirm } from '../ui.js';

const LOG_STEPS = [100, 500, 2000];
let logLines = 100;
/// Monotonic id of the newest log read; a response with an older id is dropped.
let logRequest = 0;
let logMeta = { lines: '', path: '' };
let exported = '';

const REPO = 'https://github.com/grill-glitch/uperf-libre';
const UPSTREAM = 'https://github.com/yc9559/uperf';
const DFPS = 'https://github.com/yc9559/dfps';

export function render(data, onRestart) {
    logMeta = { lines: String(data['log.lines'] || '').trim(), path: String(data['log.path'] || '').trim() };
    renderAbout();
    renderLogMeta();
    document.getElementById('current-language').textContent = currentLanguage();

    setGlyph('restart_alt', document.getElementById('restart-icon'), 24, 'dim');
    setGlyph('language', document.getElementById('language-icon'), 24, 'dim');
    document.getElementById('restart-item').onclick = () =>
        askConfirm('confirm_restart_title', 'confirm_restart_body', {}, onRestart);
    document.getElementById('language-item').onclick = () => cycleLanguage();
}

function renderAbout() {
    const about = document.getElementById('about-card');
    about.textContent = '';
    about.className = 'card flush';
    addLink(about, glyph('layers', 24, 'dim'), getString('label_platform'), 'dfps (yc9559) — Apache-2.0', DFPS);
    addLink(about, glyph('open_in_new', 24, 'dim'), getString('label_upstream'), 'uperf (yc9559)', UPSTREAM);
    addLink(about, glyph('open_in_new', 24, 'dim'), getString('label_source_repo'), 'uperf-libre', REPO);

    const license = document.createElement('div');
    license.className = 'list-item';
    const text = document.createElement('div');
    text.className = 'list-item-content';
    const title = document.createElement('div');
    title.className = 'list-title';
    title.textContent = `${getString('label_license')}: Apache-2.0`;
    const note = document.createElement('div');
    note.className = 'list-sub';
    note.textContent = getString('attr_text');
    text.append(title, note);
    license.append(text);
    about.append(license);
}

/** `iconEl` is built by the caller so the glyph name stays a literal at the call site. */
function addLink(container, iconEl, label, value, url) {
    const row = document.createElement('div');
    row.className = 'list-item';
    row.append(document.createElement('md-ripple'));
    row.append(iconEl);
    const text = document.createElement('div');
    text.className = 'list-item-content';
    const title = document.createElement('div');
    title.className = 'list-title';
    title.textContent = label;
    const detail = document.createElement('div');
    detail.className = 'list-sub';
    detail.textContent = value;
    text.append(title, detail);
    row.append(text);
    row.append(glyph('open_in_new', 18, 'dim'));
    row.onclick = async () => {
        const ok = await openUrl(url);
        if (!ok) window.__toast(getString('msg_open_fail'));
    };
    container.append(row);
}

/** Two locales, so a tap toggles; a longer list would need a picker dialog. */
async function cycleLanguage() {
    const list = availableLanguages();
    // `% 0` is NaN and `loadTranslations(undefined)` would run if the list were ever empty;
    // with one locale the tap silently reloads the same language and shows a pointless toast.
    if (list.length < 2) {
        window.__toast(currentLanguage());
        return;
    }
    const next = list[(list.indexOf(currentLanguage()) + 1) % list.length];
    try {
        await loadTranslations(next);
    } catch (err) {
        // A failed fetch used to become an unhandled rejection with the UI half-switched.
        window.__toast(err instanceof Error ? err.message : String(err));
        return;
    }
    applyStaticTranslations();
    window.__rerender();
    window.__toast(next);
}

export async function refreshLog() {
    const view = document.getElementById('log-view');
    if (!view) return;
    view.textContent = getString('msg_loading');
    // Request token: rapid step changes or repeated taps left several reads in flight, and a
    // slower earlier response could overwrite a newer (or fresher) one.
    const request = ++logRequest;
    try {
        const text = await readLog(logLines);
        if (request !== logRequest) return;
        view.textContent = text.trim() ? text : getString('msg_no_log');
    } catch (err) {
        if (request !== logRequest) return;
        view.textContent = err instanceof Error ? err.message : String(err);
    }
}

/** Log identity, plus where the last export landed (`webui.sh export`). */
function renderLogMeta() {
    const parts = [];
    if (logMeta.lines) parts.push(getString('log_meta_lines', { n: logMeta.lines }));
    if (logMeta.path) parts.push(logMeta.path);
    if (exported) parts.push(getString('log_meta_exported', { path: exported }));
    document.getElementById('log-meta').textContent = parts.join(' · ');
}

export function setupLogControls() {
    setGlyph('refresh', document.getElementById('log-refresh-icon'), 18);
    setGlyph('wrap_text', document.getElementById('log-wrap-icon'), 20);
    setGlyph('content_copy', document.getElementById('log-copy-icon'), 20);
    setGlyph('download', document.getElementById('log-export-icon'), 20);
    setGlyph('fullscreen', document.getElementById('log-fullscreen-icon'), 20);

    document.getElementById('log-refresh').onclick = () => refreshLog();
    for (const button of document.querySelectorAll('.log-lines md-text-button')) {
        button.onclick = () => {
            logLines = Number(button.dataset.lines) || LOG_STEPS[0];
            markActiveLines();
            refreshLog();
        };
    }
    const wrapButton = document.getElementById('log-wrap');
    wrapButton.onclick = toggleWrap;
    // The markup starts with wrapping on; without this the button's `active` state started
    // out wrong and the first tap turned wrapping *off* while the button turned on.
    const logView = document.getElementById('log-view');
    if (logView) wrapButton.toggleAttribute('active', logView.classList.contains('wrap'));
    document.getElementById('log-copy').onclick = copyLog;
    document.getElementById('log-export').onclick = exportLogFile;
    document.getElementById('log-fullscreen').onclick = toggleFullscreen;
    markActiveLines();
}

function markActiveLines() {
    for (const button of document.querySelectorAll('.log-lines md-text-button')) {
        button.toggleAttribute('active', Number(button.dataset.lines) === logLines);
    }
}

function toggleWrap() {
    const on = document.getElementById('log-view').classList.toggle('wrap');
    document.getElementById('log-wrap').toggleAttribute('active', on);
}

function toggleFullscreen() {
    const on = document.getElementById('log-card').classList.toggle('fullscreen');
    document.body.classList.toggle('log-open', on);
    setGlyph(on ? 'fullscreen_exit' : 'fullscreen', document.getElementById('log-fullscreen-icon'), 20);
    const label = getString(on ? 'log_exit_fullscreen' : 'log_fullscreen');
    const button = document.getElementById('log-fullscreen');
    button.setAttribute('title', label);
    button.setAttribute('aria-label', label);
}

async function copyLog() {
    const view = document.getElementById('log-view');
    const text = view.textContent || '';
    if (!text.trim()) {
        window.__toast(getString('msg_no_log'));
        return;
    }
    if (await writeClipboard(text)) {
        window.__toast(getString('toast_copied'));
        return;
    }
    // A WebView may refuse both clipboard paths. Rather than fail silently, leave the
    // text selected so the long-press menu can still copy it.
    window.__toast(getString(selectText(view) ? 'toast_copy_select' : 'toast_copy_fail'));
}

function selectText(el) {
    try {
        const range = document.createRange();
        range.selectNodeContents(el);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        return true;
    } catch {
        return false;
    }
}

/**
 * `navigator.clipboard` needs a secure context and a user gesture, and a WebView can
 * refuse it silently; the selection-based path is the fallback that still works there.
 */
async function writeClipboard(text) {
    try {
        if (navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
            return true;
        }
    } catch {
        /* fall through */
    }
    // The hidden node must be removed on every path: a locked-down WebView throws in
    // `select()`/`execCommand`, and the old code returned from `catch` with a full log still
    // attached to the document (a new one per failed copy).
    let area = null;
    try {
        area = document.createElement('textarea');
        area.value = text;
        area.setAttribute('readonly', '');
        area.style.position = 'fixed';
        area.style.top = '-1000px';
        area.style.opacity = '0';
        document.body.append(area);
        area.select();
        return document.execCommand('copy');
    } catch {
        return false;
    } finally {
        area?.remove();
    }
}

async function exportLogFile() {
    const button = document.getElementById('log-export');
    button.disabled = true;
    try {
        const result = await exportLog(logLines);
        if (result['export.ok'] !== '1') {
            throw new Error(result['export.error'] || 'export failed');
        }
        exported = result['export.path'] || '';
        renderLogMeta();
        window.__toast(getString('toast_export_ok', { path: exported }));
    } catch (err) {
        window.__toast(getString('toast_export_fail', { err: err.message }));
    } finally {
        button.disabled = false;
    }
}

/** Paths the about card documents, exported for the tests to assert against. */
export const paths = { MODULE_DIR, USER_PATH };
