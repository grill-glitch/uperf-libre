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

/**
 * Human label for a state value the scripts emit (`running`, `gave-up`, `restarting`
 * …). Unknown values are shown as-is: inventing a label would hide a state the scripts
 * grew that this page does not know about yet.
 */
function stateLabel(state) {
    const key = `state_${String(state).replace(/-/g, '_')}`;
    const text = getString(key);
    return text === key ? state : text;
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

    // Supervision: what the daemon claims (`uperf.state`) and what the watchdog says
    // (`uperf_watchdog.state`). The two together are what make a kill visible — a
    // stale `daemon.state=running` with no process is the documented signature of one,
    // and `watchdog.state=gave-up` means the restart budget was spent and the platform
    // governor is in charge on purpose (docs/m9-watchdog.md).
    const supervision = document.getElementById('supervision-card');
    supervision.textContent = '';
    const wdState = String(data['watchdog.state'] || '').trim();
    const claim = String(data['daemon.state'] || '').trim();
    const killed = claim === 'running' && !running;
    const rows = [
        [getString('label_watchdog_state'), wdState ? stateLabel(wdState) : getString('state_unknown')],
        [getString('label_watchdog_pid'), data['watchdog.pid']],
        [getString('label_watchdog_restarts'), data['watchdog.restarts']],
        [getString('label_daemon_claim'), killed
            ? getString('daemon_claim_killed')
            : (claim ? stateLabel(claim) : '')],
        [getString('label_armed_clusters'), data['daemon.armed']],
    ];
    // The frame source only when it is being supervised: `off` means the module is not
    // injecting into surfaceflinger, and a row saying so every time would be noise.
    const sfState = String(data['watchdog.sf_state'] || '').trim();
    if (sfState && sfState !== 'off') {
        rows.push([getString('label_frame_source'), stateLabel(sfState)]);
        rows.push([getString('label_frame_injected'), data['watchdog.sf_injects'] || '0']);
        rows.push([getString('label_frame_failed'), data['watchdog.sf_fails'] || '0']);
    }
    // ⑤ the daemon's own frame leg (opt-in `UPERF_SF_BINDER=1`): `hint` while the
    // injected hint is fresh, `fps` when the direct-binder leg had to take over.
    const leg = String(data['frame.source'] || '').trim();
    if (leg) {
        rows.push([getString('label_frame_leg'), leg]);
        const fps = String(data['frame.fps'] || '').trim();
        if (fps && fps !== '-') {
            rows.push([getString('label_frame_fps'), fps]);
        }
    }
    addRow(supervision, 'verified_user', rows, getString('section_supervision'));

    const device = document.getElementById('device-card');
    device.textContent = '';
    addRow(device, 'memory', [
        [getString('label_model'), data['device.model']],
        [getString('label_soc'), [data['soc.platform'], data['soc.model']].filter(Boolean).join(' / ')],
        [getString('label_android'), [data['android.release'], data['android.sdk'] ? `API ${data['android.sdk']}` : ''].filter(Boolean).join(' · ')],
        [getString('label_kernel'), data['kernel.release']],
        [getString('label_selinux'), data['selinux']],
        // ⑩: the module ships arm64-only binaries, so say it rather than let a user
        // wonder why nothing runs.
        [getString('label_abi'), data['device.abi']
            ? (data['module.arch_supported'] === '1'
                ? data['device.abi']
                : `${data['device.abi']} (${getString('value_unsupported')})`)
            : ''],
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
