// Tab 1 — work state, the loaded config, the device and the module.
//
// Every value shown here comes from `webui.sh` (already verified from a shell) or from
// the config JSON the daemon itself loads. Nothing is estimated: when a field is
// missing the row is omitted rather than filled with a guess.

import { getString } from '../language.js';

function row(iconEl, container) {
    const wrap = document.createElement('div');
    wrap.className = 'row';
    if (iconEl) wrap.append(iconEl);
    const text = document.createElement('div');
    text.className = 'row-text';
    wrap.append(text);
    container.append(wrap);
    return text;
}

function addDetail(textEl, label, value) {
    if (value === undefined || value === null || value === '') return;
    const line = document.createElement('div');
    line.className = 'row-detail';
    line.textContent = `${label}: ${value}`;
    textEl.append(line);
}

function formatUptime(seconds) {
    const s = Number(seconds);
    if (!Number.isFinite(s) || s <= 0) return '';
    const d = Math.floor(s / 86400);
    const h = Math.floor((s % 86400) / 3600);
    const m = Math.floor((s % 3600) / 60);
    if (d) return `${d}d ${h}h`;
    if (h) return `${h}h ${m}m`;
    return `${m}m`;
}

function basename(path) {
    return String(path || '').split('/').filter(Boolean).pop() || '';
}

export function render(data, config, error) {
    const statusCard = document.getElementById('status-card');
    const title = document.getElementById('status-title');
    const subtitle = document.getElementById('status-subtitle');
    const badge = document.getElementById('gov-badge');

    if (error) {
        // One honest place to show a failure, instead of a page of empty values.
        statusCard.classList.remove('ok');
        statusCard.classList.add('down');
        title.textContent = error.includes('ksu is not defined')
            ? getString('msg_no_ksu')
            : getString('msg_load_fail', { err: error });
        subtitle.textContent = '';
        badge.textContent = '';
        badge.classList.remove('on');
        return;
    }

    const running = Number(data['daemon.count'] || 0) > 0;
    statusCard.classList.toggle('ok', running);
    statusCard.classList.toggle('down', !running);
    title.textContent = running ? getString('state_running') : getString('state_stopped');
    subtitle.textContent = running
        ? getString('state_running_detail', {
            pids: (data['daemon.pids'] || '').trim(),
            uptime: formatUptime(data['uptime.sec']),
        })
        : getString('state_stopped_detail');

    const takeover = data['governor.takeover'] === '1';
    badge.textContent = takeover ? getString('badge_governor_on') : getString('badge_governor_off');
    badge.classList.toggle('on', takeover);

    document.getElementById('cfg-name').textContent = basename(data['config.path']) || '—';
    document.getElementById('cfg-author').textContent = config
        ? `${config.meta?.name ?? ''}`
        : getString('msg_no_config', { path: `${data['user.path']}/uperf.json` });
    document.getElementById('cfg-extra').textContent = [
        data['preset.present'] === '1'
            ? `${getString('label_preset')}: ${data['preset.current']}`
            : `${getString('label_preset')}: ${getString('mode_current_none')}`,
        data['config.present'] === '1'
            ? `${getString('label_sha')}: ${String(data['config.sha256'] || '').slice(0, 16)}`
            : '',
    ].filter(Boolean).join(' · ');

    const device = document.getElementById('device-card');
    device.textContent = '';
    addRow(device, 'memory', [
        [getString('label_model'), data['device.model']],
        [getString('label_soc'), [data['soc.platform'], data['soc.model']].filter(Boolean).join(' / ')],
        [getString('label_android'), [data['android.release'], data['android.sdk'] ? `API ${data['android.sdk']}` : ''].filter(Boolean).join(' · ')],
        [getString('label_kernel'), data['kernel.release']],
        [getString('label_selinux'), data['selinux']],
    ]);

    const module = document.getElementById('module-card');
    module.textContent = '';
    addRow(module, 'package_2', [
        [getString('label_author'), data['module.author']],
        [getString('label_description'), data['module.description']],
    ], `${data['module.name'] || ''}`, [data['module.version'], data['module.versioncode'] ? `(${data['module.versioncode']})` : ''].filter(Boolean).join(' '));
}

/** A card with a leading icon, an optional title/subtitle, and label/value rows. */
function addRow(container, iconName, pairs, heading, subheading) {
    const wrap = document.createElement('div');
    wrap.className = 'row';
    const glyph = document.createElement('md-icon');
    glyph.className = 'row-icon';
    glyph.innerHTML = window.__icon(iconName);
    wrap.append(glyph);
    const text = document.createElement('div');
    text.className = 'row-text';
    if (heading) {
        const h = document.createElement('div');
        h.className = 'row-title';
        h.textContent = heading;
        text.append(h);
    }
    if (subheading) {
        const s = document.createElement('div');
        s.className = 'row-detail';
        s.textContent = subheading;
        text.append(s);
    }
    for (const [label, value] of pairs) addDetail(text, label, value);
    wrap.append(text);
    container.append(wrap);
}
