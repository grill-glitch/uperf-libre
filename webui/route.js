// Page switching and the bottom bar. Follows the reference implementation's shape
// (KernelSU-Next/KPatch-Next-Module, webui/route.js, MIT).

import { getString } from './language.js';

const PAGES = {
    home: { id: 'home-page', title: () => 'Uperf' },
    mode: { id: 'mode-page', title: () => getString('tab_mode') },
    more: { id: 'more-page', title: () => getString('tab_more') },
};

let activeTab = 'home';

export function currentTab() {
    return activeTab;
}

/** Shows one page and keeps the bottom bar in sync. */
export function switchTo(tab) {
    const page = PAGES[tab];
    if (!page) return;
    activeTab = tab;

    for (const el of document.querySelectorAll('.page')) {
        el.classList.toggle('active', el.id === page.id);
    }
    document.getElementById('title').textContent = page.title();
    for (const item of document.querySelectorAll('.bottom-bar-item')) {
        item.toggleAttribute('selected', item.id === tab);
    }
    document.getElementById('content').scrollTop = 0;
}

/** The manager provides `ksu.exit()`; only show the button when it can work. */
function setupExitButton() {
    const exit = typeof window.ksu?.exit === 'function'
        ? () => window.ksu.exit()
        : (typeof window.webui?.exit === 'function' ? () => window.webui.exit() : null);
    const button = document.getElementById('back-btn');
    if (!exit) {
        button.classList.add('hidden');
        button.onclick = null;
        return;
    }
    button.classList.remove('hidden');
    button.onclick = (event) => {
        event.stopPropagation();
        setTimeout(exit, 0);
    };
}

/** `onSwitch` runs on every tab change, so a page can load lazily. */
export function setupRoute(onSwitch) {
    for (const item of document.querySelectorAll('.bottom-bar-item')) {
        item.addEventListener('click', () => {
            if (item.id === activeTab) return;
            switchTo(item.id);
            onSwitch?.(item.id);
        });
    }
    setupExitButton();
    switchTo('home');
}
