// Localisation, same shape as the reference implementation
// (KernelSU-Next/KPatch-Next-Module, webui/language.js, MIT): one small
// `<resources><string name="…">` file per locale under `public/locales/strings/`.
//
// Placeholders are named (`{path}`, `{pids}`) and filled from a params object, since
// the strings here need several of them and positional arguments made the call sites
// unreadable.

const AVAILABLE = ['en', 'zh-CN'];
const STORAGE_KEY = 'uperf.lang';

let current = 'en';
let strings = {};

export function availableLanguages() {
    return AVAILABLE;
}

export function currentLanguage() {
    return current;
}

/** `localStorage` can be unavailable in a locked-down WebView; never let it throw. */
function storedLanguage() {
    try {
        return localStorage.getItem(STORAGE_KEY);
    } catch {
        return null;
    }
}

function storeLanguage(code) {
    try {
        localStorage.setItem(STORAGE_KEY, code);
    } catch {
        /* ignore */
    }
}

function pickLanguage(preferred) {
    if (preferred && AVAILABLE.includes(preferred)) return preferred;
    const saved = storedLanguage();
    if (saved && AVAILABLE.includes(saved)) return saved;
    // `navigator` may be absent (worker/SSR/test) and `navigator.language` is not guaranteed
    // to be a string.
    const nav = String(globalThis.navigator?.language || 'en').toLowerCase();
    return nav.startsWith('zh') ? 'zh-CN' : 'en';
}

export async function loadTranslations(preferred) {
    const code = pickLanguage(preferred);
    const res = await fetch(`./locales/strings/${code}.xml`);
    if (!res.ok) {
        if (code === 'en') throw new Error(`locale ${code}: HTTP ${res.status}`);
        // Any other locale falls back to the source language instead of leaving the UI on
        // the previous one (or on raw keys) with an unhandled rejection.
        return loadTranslations('en');
    }
    const doc = new DOMParser().parseFromString(await res.text(), 'application/xml');
    // parseFromString does not throw on malformed XML: it returns a document with a
    // <parsererror>. Without this check a truncated locale file produced an empty table
    // that was then committed, and the whole UI rendered raw keys.
    if (doc.querySelector('parsererror')) {
        throw new Error(`locale ${code}: malformed XML`);
    }
    const table = {};
    for (const node of doc.querySelectorAll('string')) {
        table[node.getAttribute('name')] = node.textContent;
    }
    strings = table;
    current = code;
    storeLanguage(code);
    return code;
}

/** A missing key returns the key itself, so an untranslated string is visible. */
export function getString(key, params) {
    let text = strings[key] ?? key;
    if (params) {
        // One pass: substituting sequentially let a value that itself contains a
        // placeholder token be expanded by a later iteration.
        text = text.replace(/\{([A-Za-z0-9_]+)\}/g, (match, name) =>
            params[name] !== undefined ? String(params[name]) : match);
    }
    return text;
}

/** Fills every element that carries `data-i18n` (and the attribute variants). */
export function applyStaticTranslations(root = document) {
    for (const el of root.querySelectorAll('[data-i18n]')) {
        // `textContent =` replaces every child node: only safe while the element's whole
        // content is the label (an icon+label control must use `data-i18n-title`).
        if (el.childElementCount === 0) el.textContent = getString(el.dataset.i18n);
    }
    // Icon-only controls have no room for a label, so their accessible name and their
    // long-press tooltip are set from the same locale key as everything else.
    for (const el of root.querySelectorAll('[data-i18n-title]')) {
        const text = getString(el.dataset.i18nTitle);
        el.setAttribute('title', text);
        if (!el.hasAttribute('aria-label')) el.setAttribute('aria-label', text);
    }
    // `md-outlined-text-field` takes its label property, not its text content.
    for (const el of root.querySelectorAll('[data-i18n-label]')) {
        el.label = getString(el.dataset.i18nLabel);
    }
    // Resolve the owning document from `root`: a scoped root (dialog, test fixture) must not
    // mutate the global page.
    const doc = root.nodeType === Node.DOCUMENT_NODE ? root : root.ownerDocument;
    if (doc?.documentElement) doc.documentElement.lang = current;
}
