import { defineConfig } from 'vite';

// The module tree is the build target: KernelSU serves `<module>/webroot` as the
// WebUI, so the built assets land straight into the module directory and `build.sh
// pack` picks them up with everything else. `base: './'` is required because the
// manager serves the files from a path that is not the server root.
export default defineConfig({
    base: './',
    build: {
        outDir: '../magisk/webroot',
    },
});
