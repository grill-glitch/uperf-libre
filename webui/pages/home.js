// Tab 1 — work state, the loaded config + quick preset toggle, supervision, and the
// static device/module facts behind a disclosure.
//
// Every value shown here comes from `webui.sh` (already verified from a shell) or from
// the config JSON the daemon itself loads. Nothing is estimated: when a field is
// missing the row is omitted rather than filled with a guess.
//
// Layout rule this page follows: what changes goes above the fold and gets the numbers
// treatment; what is fixed about the phone (SoC, kernel, ABI, module author) is
// reference material and stays collapsed. The previous version interleaved both at equal
// weight, which is why the fold was spent on a kernel string.

import { getString } from '../language.js';
import { glyph, setGlyph, kvRows, openInfo } from '../ui.js';
import { USER_PATH } from '../ctl.js';
import { presetNames, shortPresetLabel, applyPreset } from '../presets.js';

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

export function render(data, config, error, onPresetChanged) {
    renderHero(data, error);

    // Without the bridge there is no data at all: `webui.sh` is the only source for the
    // config, the supervision rows and the device facts, so the rest of the page would be
    // a grid of empty values. One honest message in the hero beats four blank cards.
    const hasData = !error;
    for (const id of ['config-label', 'config-card', 'supervision-label', 'supervision-card',
                      'device-toggle', 'device-body']) {
        document.getElementById(id).hidden = !hasData;
    }
    if (!hasData) return;

    renderConfig(data, config, onPresetChanged);
    renderSupervision(data);
    renderDeviceModule(data);
    setupDeviceToggle();
    document.getElementById('cfg-hint').textContent = getString('mode_hint_inode', {
        path: `${USER_PATH}/cur_powermode.txt`,
    });
}

function renderHero(data, error) {
    const card = document.getElementById('status-card');
    const title = document.getElementById('status-title');
    const sub = document.getElementById('status-subtitle');
    const badge = document.getElementById('gov-badge');
    const stats = document.getElementById('status-stats');

    if (error) {
        // One honest place to show a failure, instead of a page of empty values.
        card.classList.remove('ok');
        card.classList.add('down');
        setGlyph('error', document.getElementById('status-icon'), 24);
        title.textContent = error.includes('ksu is not defined')
            ? getString('msg_no_ksu')
            : getString('msg_load_fail', { err: error });
        sub.textContent = '';
        badge.hidden = true;
        stats.hidden = true;
        return;
    }

    badge.hidden = false;
    stats.hidden = false;

    const running = Number(data['daemon.count'] || 0) > 0;
    card.classList.toggle('ok', running);
    card.classList.toggle('down', !running);
    setGlyph(running ? 'speed' : 'warning', document.getElementById('status-icon'), 24);
    title.textContent = running ? getString('state_running') : getString('state_stopped');
    sub.textContent = running
        ? getString('state_running_detail', {
            pids: (data['daemon.pids'] || '').trim(),
            uptime: formatUptime(data['uptime.sec']),
        })
        : getString('state_stopped_detail');

    renderStats(data);
    renderGovernorBadge(data);
}

/**
 * The three numbers that move. Pids and armed-cluster count are what the watchdog and
 * the daemon status file report, so they are shown verbatim — a dash when the script
 * printed nothing, never a zero standing in for "unknown".
 */
function renderStats(data) {
    const row = document.getElementById('status-stats');
    row.textContent = '';
    const pids = (data['daemon.pids'] || '').trim();
    const armed = String(data['daemon.armed'] ?? '').trim();
    const uptime = formatUptime(data['uptime.sec']);
    row.append(
        statTile(getString('stat_uptime'), uptime || '—'),
        statTile(getString('stat_pids'), pids || '—'),
        statTile(getString('stat_armed'), armed || '—'),
    );
}

function statTile(label, value) {
    const el = document.createElement('div');
    el.className = 'stat';
    const l = document.createElement('div');
    l.className = 'stat-label';
    l.textContent = label;
    const v = document.createElement('div');
    v.className = 'stat-value';
    v.textContent = value;
    el.append(l, v);
    return el;
}

/**
 * The governor badge is interactive on purpose. It reads as a state, but the question it
 * provokes ("why is the tuner not touching the governor?") is the one thing on this
 * screen a user cannot answer from the page — so tapping it explains what takeover
 * means, why it is off by default on this kernel, and what the policies currently are.
 */
function renderGovernorBadge(data) {
    const badge = document.getElementById('gov-badge');
    const takeover = data['governor.takeover'] === '1';
    badge.classList.toggle('on', takeover);
    document.getElementById('gov-badge-text').textContent =
        takeover ? getString('badge_governor_on') : getString('badge_governor_off');
    setGlyph('info', document.getElementById('gov-badge-icon'), 14);
    badge.onclick = () => openInfo('badge_info_title', [
        { text: 'badge_info_what' },
        { text: takeover ? 'badge_info_on' : 'badge_info_off' },
        { text: 'badge_info_policies', params: { policies: String(data['governor.policies'] || '—').trim() } },
        { text: 'badge_info_recorded', params: { recorded: String(data['governor.recorded'] || '—').trim() } },
    ]);
}

function renderConfig(data, config, onPresetChanged) {
    kvRows(document.getElementById('config-kv'), [
        [getString('label_config'), basename(data['config.path'])],
        [getString('label_meta'), config?.meta?.name ?? getString('msg_no_config', { path: `${USER_PATH}/uperf.json` })],
        [getString('label_sha'), data['config.present'] === '1' ? String(data['config.sha256'] || '').slice(0, 16) : '', { mono: true }],
        [getString('label_path'), data['config.path'], { mono: true }],
    ]);

    const chips = document.getElementById('preset-chips');
    chips.textContent = '';
    chips.hidden = !config;
    if (!config) return;

    const current = data['preset.present'] === '1' ? data['preset.current'] : '';
    for (const name of presetNames(config)) {
        const chip = document.createElement('div');
        chip.className = 'chip';
        chip.setAttribute('role', 'button');
        chip.setAttribute('aria-current', String(name === current));
        chip.append(document.createElement('md-ripple'));
        if (name === current) chip.append(glyph('check_circle', 16));
        const label = document.createElement('span');
        label.textContent = shortPresetLabel(name);
        chip.append(label);
        chip.onclick = () => applyPreset(name, chip, onPresetChanged);
        chips.append(chip);
    }
}

/**
 * What the daemon claims (`uperf.state`) and what the watchdog says
 * (`uperf_watchdog.state`). The two together are what make a kill visible — a stale
 * `daemon.state=running` with no process is the documented signature of one, and
 * `watchdog.state=gave-up` means the restart budget was spent and the platform governor
 * is in charge on purpose (docs/m9-watchdog.md).
 */
function renderSupervision(data) {
    const running = Number(data['daemon.count'] || 0) > 0;
    const wdState = String(data['watchdog.state'] || '').trim();
    const claim = String(data['daemon.state'] || '').trim();
    const killed = claim === 'running' && !running;

    const pairs = [
        [getString('label_watchdog_state'), wdState ? stateLabel(wdState) : getString('state_unknown')],
        [getString('label_watchdog_pid'), (data['watchdog.pid'] || '').trim()],
        [getString('label_watchdog_restarts'), (data['watchdog.restarts'] || '').trim()],
        [getString('label_daemon_claim'), killed ? getString('daemon_claim_killed') : (claim ? stateLabel(claim) : '')],
    ];

    // The frame source only when it is being supervised: `off` means the module is not
    // injecting into surfaceflinger, and a row saying so every time would be noise.
    const sfState = String(data['watchdog.sf_state'] || '').trim();
    if (sfState && sfState !== 'off') {
        pairs.push([getString('label_frame_source'), stateLabel(sfState)]);
        pairs.push([getString('label_frame_injected'), (data['watchdog.sf_injects'] || '0').trim()]);
        pairs.push([getString('label_frame_failed'), (data['watchdog.sf_fails'] || '0').trim()]);
    }
    // ⑤ the daemon's own frame leg (opt-in `UPERF_SF_BINDER=1`): `hint` while the
    // injected hint is fresh, `fps` when the direct-binder leg had to take over.
    const leg = String(data['frame.source'] || '').trim();
    if (leg) {
        pairs.push([getString('label_frame_leg'), leg]);
        const fps = String(data['frame.fps'] || '').trim();
        if (fps && fps !== '-') pairs.push([getString('label_frame_fps'), fps]);
    }

    kvRows(document.getElementById('supervision-kv'), pairs);
}

function renderDeviceModule(data) {
    // ⑩: the module ships arm64-only binaries, so say it rather than let a user
    // wonder why nothing runs.
    const abi = data['device.abi']
        ? (data['module.arch_supported'] === '1' ? data['device.abi'] : `${data['device.abi']} (${getString('value_unsupported')})`)
        : '';

    kvRows(document.getElementById('device-kv'), [
        [getString('label_model'), (data['device.model'] || '').trim()],
        [getString('label_soc'), [data['soc.platform'], data['soc.model']].filter(Boolean).join(' / ')],
        [getString('label_android'), [data['android.release'], data['android.sdk'] ? `API ${data['android.sdk']}` : ''].filter(Boolean).join(' · ')],
        [getString('label_kernel'), (data['kernel.release'] || '').trim(), { mono: true }],
        [getString('label_selinux'), (data['selinux'] || '').trim()],
        [getString('label_abi'), abi],
    ]);

    kvRows(document.getElementById('module-kv'), [
        [getString('label_name'), (data['module.name'] || '').trim()],
        [getString('label_version'), [(data['module.version'] || '').trim(), data['module.versioncode'] ? `(${data['module.versioncode'].trim()})` : ''].filter(Boolean).join(' ')],
        [getString('label_author'), (data['module.author'] || '').trim()],
        [getString('label_description'), (data['module.description'] || '').trim()],
        [getString('label_module_dir'), (data['module.dir'] || '').trim(), { mono: true }],
    ]);
}

/** Open/closed state lives on the body element, so a re-render does not reset it. */
function setupDeviceToggle() {
    const toggle = document.getElementById('device-toggle');
    const body = document.getElementById('device-body');
    const label = document.getElementById('device-toggle-label');
    setGlyph('devices', document.getElementById('device-toggle-icon'), 20, 'dim');
    label.textContent = getString('section_device_module');

    const paint = () => {
        const open = body.classList.contains('open');
        setGlyph(open ? 'keyboard_arrow_up' : 'keyboard_arrow_down', document.getElementById('device-toggle-chev'), 20, 'chev');
        toggle.setAttribute('aria-expanded', String(open));
    };
    toggle.onclick = () => {
        body.classList.toggle('open');
        paint();
    };
    paint();
}
