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
    const nav = (navigator.language || 'en').toLowerCase();
    return nav.startsWith('zh') ? 'zh-CN' : 'en';
}

export async function loadTranslations(preferred) {
    const code = pickLanguage(preferred);
    const res = await fetch(`./locales/strings/${code}.xml`);
    if (!res.ok) throw new Error(`locale ${code}: HTTP ${res.status}`);
    const doc = new DOMParser().parseFromString(await res.text(), 'application/xml');
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
        for (const [name, value] of Object.entries(params)) {
            text = text.split(`{${name}}`).join(String(value));
        }
    }
    return text;
}

/** Fills every element that carries `data-i18n`. */
export function applyStaticTranslations(root = document) {
    for (const el of root.querySelectorAll('[data-i18n]')) {
        el.textContent = getString(el.dataset.i18n);
    }
    document.documentElement.lang = current;
}
