import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

// The module tree is the build target: KernelSU serves `<module>/webroot` as the
// WebUI, so the built assets land straight into the module directory and `build.sh
// pack` picks them up with everything else. `base: './'` is required because the
// manager serves the files from a path that is not the server root.
export default defineConfig({
    base: './',
    build: {
        // Resolved against this file, not the caller's cwd: `vite build --config
        // webui/vite.config.js` from the repo root used to resolve the destination to
        // `<repo>/../magisk/webroot`, outside the repository — and `--emptyOutDir` then
        // emptied whatever directory it landed on.
        outDir: fileURLToPath(new URL('../magisk/webroot', import.meta.url)),
        // Declared here so the behaviour does not depend on the `--emptyOutDir` flag in
        // package.json: stale hashed chunks left in webroot would be packed into the module.
        emptyOutDir: true,
        // The bundle runs in the device's system WebView, which on the old non-GMS devices
        // this module targets can be far older than Vite 7's default target.
        target: ['chrome61'],
    },
});
