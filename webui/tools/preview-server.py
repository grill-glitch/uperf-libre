#!/usr/bin/env python3
"""Serve `magisk/webroot` the way the KernelSU manager does, with a preview bridge.

The manager serves the WebUI inside a WebView and injects `window.ksu`, which is what
lets the page run commands as root. Outside the manager that global does not exist, so
the page cannot render anything real: every read throws and the page correctly shows
"open this page from the KernelSU manager". That is the right behaviour on a device and
useless for looking at the UI.

This script injects a stand-in bridge into `index.html` **before** the app bundle, so the
page runs unmodified and renders from payloads that were captured from a real device.
Nothing here writes to the device: it is a viewer, not a test rig. Real-device checks
belong to adb.

    # capture the payloads off a rooted device once, then serve
    python3 tools/preview-server.py --capture --serial <serial> --port 8097

    # or serve previously captured payloads
    python3 tools/preview-server.py --payloads .preview-payloads --port 8097

Open http://127.0.0.1:8097/ (it binds 0.0.0.0, so the phone's browser can open
http://<pc-lan-ip>:8097/ too). The theme follows the browser's `prefers-color-scheme`;
the CSS is otherwise driven by the manager's own colour variables, and theme.css gives
every one of them a literal Material 3 fallback.

The payloads are the module's own control-script output, so the page sees exactly the
`key=value` shapes it sees on the device:

    payloads/all.txt      sh webui.sh all
    payloads/log100.txt   sh webui.sh log 100
    payloads/log500.txt   sh webui.sh log 500
    payloads/uperf.json   cat /sdcard/Android/yc/uperf/uperf.json
    payloads/dfps.txt     sh dfps.sh status; echo ---; sh dfps.sh info

Writes (`set-preset`, `set-rule`, `restart`, `export`) are answered from the script's
documented output shape; `set-preset` is stateful so the page's read-back behaviour
("redraw from what the file now contains") can be exercised without a device.
"""

import argparse
import http.server
import io
import json
import os
import shutil
import socketserver
import subprocess
import sys

MODULE_DIR = "/data/adb/modules/uperf"
USER_PATH = "/sdcard/Android/yc/uperf"

CAPTURE = {
    "all.txt": f"sh {MODULE_DIR}/script/webui.sh all",
    "log100.txt": f"sh {MODULE_DIR}/script/webui.sh log 100",
    "log500.txt": f"sh {MODULE_DIR}/script/webui.sh log 500",
    "uperf.json": f"cat {USER_PATH}/uperf.json",
    "dfps.txt": f"sh {MODULE_DIR}/script/dfps.sh status; echo ---; sh {MODULE_DIR}/script/dfps.sh info",
}


def capture(serial, out_dir):
    adb = shutil.which("adb") or os.path.expanduser("~/Android/Sdk/platform-tools/adb")
    if not os.path.exists(adb):
        sys.exit("preview-server: adb not found; pass --payloads with a captured set")
    os.makedirs(out_dir, exist_ok=True)
    for name, command in CAPTURE.items():
        cmd = [adb]
        if serial:
            cmd += ["-s", serial]
        cmd += ["shell", f"su -c {json.dumps(command)}"]
        result = subprocess.run(cmd, capture_output=True, text=True)
        text = result.stdout
        if not text.strip():
            sys.exit(f"preview-server: capture of {name} was empty ({result.stderr.strip()})")
        with open(os.path.join(out_dir, name), "w") as fh:
            fh.write(text)
        print(f"  captured {name} ({len(text)} bytes)")


def build_bridge(payloads):
    def read(name):
        path = os.path.join(payloads, name)
        if not os.path.exists(path):
            sys.exit(f"preview-server: missing {path}; run with --capture first")
        with open(path) as fh:
            return fh.read()

    all_text = read("all.txt")
    log100, log500 = read("log100.txt"), read("log500.txt")
    config, dfps = read("uperf.json"), read("dfps.txt")
    dfps_status, _, dfps_info = dfps.partition("---")

    preset = ""
    user_path = USER_PATH
    for line in all_text.splitlines():
        if line.startswith("preset.current="):
            preset = line.split("=", 1)[1].strip()
        elif line.startswith("user.path="):
            user_path = line.split("=", 1)[1].strip()

    script = """
<script>
(function(){
  var STATE = { preset: %(preset)s, userPath: %(user_path)s, rules: %(rules)s };
  window.__PREVIEW_LOG = [];
  // The dump is captured verbatim; only `preset.current` is re-derived per call, so a
  // preset write shows up in the next read — the property the page depends on.
  function withPreset(t) {
    return t.replace(/^preset\\.current=.*$/m, 'preset.current=' + STATE.preset);
  }
  function reply(cmd){
    var c = cmd.trim();
    window.__PREVIEW_LOG.push(c);
    if (c.indexOf('script/webui.sh') !== -1) {
      var sub = c.match(/webui\\.sh +(\\S+)/);
      sub = sub ? sub[1] : '';
      if (sub === 'all' || sub === 'status' || sub === 'info') return { o: withPreset(%(all)s), e: '', n: 0 };
      if (sub === 'log') {
        var n = parseInt(((c.match(/log +'?(\\d+)'?/) || [])[1] || '100'), 10);
        return { o: (n > 100 ? %(log500)s : %(log100)s), e: '', n: 0 };
      }
      if (sub === 'set-preset') {
        var want = (c.match(/set-preset +'([^']*)'/) || [])[1];
        if (want) STATE.preset = want;
        return { o: 'preset.want=' + want + '\\npreset.set=' + want + '\\npreset.ok=1', e: '', n: 0 };
      }
      if (sub === 'restart') return { o: 'restart.before=2\\nrestart.after=2\\nrestart.ok=1', e: '', n: 0 };
      if (sub === 'export') {
        var m = ((c.match(/export +'?(\\d+)'?/) || [])[1] || '2000');
        return { o: 'export.path=' + STATE.userPath + '/export/uperf_log_preview.txt\\nexport.lines=' + m +
                   '\\nexport.bytes=298463\\nexport.ok=1', e: '', n: 0 };
      }
      return { o: '', e: 'preview: unhandled webui.sh subcommand: ' + sub, n: 1 };
    }
    if (c.indexOf('dfps.sh') !== -1) {
      var d = c.match(/dfps\\.sh +(\\S+)/);
      d = d ? d[1] : '';
      if (d === 'status') return { o: %(dfps_status)s, e: '', n: 0 };
      if (d === 'info') return { o: %(dfps_info)s, e: '', n: 0 };
      if (d === 'set-rule') return { o: 'rule.ok=1', e: '', n: 0 };
      return { o: '', e: 'preview: unhandled dfps.sh subcommand: ' + d, n: 1 };
    }
    if (c.indexOf('cat ') === 0 && c.indexOf('uperf.json') !== -1) return { o: %(config)s, e: '', n: 0 };
    if (c.indexOf('am start') === 0) return { o: '', e: '', n: 0 };
    return { o: '', e: 'preview: unhandled cmd: ' + c, n: 1 };
  }
  window.ksu = {
    exec: function(cmd, opts, cb){
      if (typeof opts === 'function') { cb = opts; }
      var r = reply(cmd);
      setTimeout(function(){ if (typeof window[cb] === 'function') window[cb](r.n, r.o, r.e); }, 0);
    },
    toast: function(m){ window.__PREVIEW_LOG.push('TOAST: ' + m); console.log('TOAST: ' + m); },
    fullScreen: function(){},
    exit: function(){ window.__PREVIEW_LOG.push('EXIT'); },
    moduleInfo: function(){ return { id: 'uperf', name: 'uperf', version: 'preview' }; }
  };
})();
</script>
""" % {
        "preset": json.dumps(preset),
        "user_path": json.dumps(user_path),
        "rules": json.dumps([p for p in dfps_info.split("rules=")[-1].split("|") if p]),
        "all": json.dumps(all_text),
        "log100": json.dumps(log100),
        "log500": json.dumps(log500),
        "config": json.dumps(config),
        "dfps_status": json.dumps(dfps_status.strip() + "\n"),
        "dfps_info": json.dumps(dfps_info.strip() + "\n"),
    }
    return script.encode()


def make_handler(dist, bridge):
    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *a, **kw):
            super().__init__(*a, directory=dist, **kw)

        def log_message(self, *a, **kw):
            pass

        def send_head(self):
            path = self.translate_path(self.path)
            if os.path.isdir(path):
                path = os.path.join(path, "index.html")
            if not os.path.isfile(path):
                self.send_error(404)
                return None
            with open(path, "rb") as fh:
                data = fh.read()
            # The manager may serve either entry point, so both get the bridge.
            if os.path.basename(path) in ("index.html", "index.txt"):
                data = data.replace(b"</head>", bridge + b"</head>", 1)
            self.send_response(200)
            self.send_header("Content-Type", self.guess_type(path))
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            return io.BytesIO(data)

    return Handler


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    default_dist = os.path.join(here, "..", "..", "magisk", "webroot")
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dist", default=os.path.normpath(default_dist), help="built webroot (default: magisk/webroot)")
    parser.add_argument("--payloads", default=".preview-payloads", help="directory holding the captured payloads")
    parser.add_argument("--capture", action="store_true", help="capture the payloads from a device first")
    parser.add_argument("--serial", default=None, help="adb serial for --capture")
    parser.add_argument("--port", type=int, default=8097)
    args = parser.parse_args()

    if args.capture:
        capture(args.serial, args.payloads)
    if not os.path.isfile(os.path.join(args.dist, "index.html")):
        sys.exit(f"preview-server: no built WebUI at {args.dist}; run `npm run build` in webui/")

    bridge = build_bridge(args.payloads)
    socketserver.TCPServer.allow_reuse_address = True
    with socketserver.TCPServer(("0.0.0.0", args.port), make_handler(args.dist, bridge)) as httpd:
        print(f"serving {args.dist} on http://0.0.0.0:{args.port}/ (bridge from {args.payloads})")
        httpd.serve_forever()


if __name__ == "__main__":
    main()
