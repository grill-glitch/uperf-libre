# T13 — Build wiring: build.sh for dfps-rs

## Question

How does dfps-rs integrate into build.sh?

1. Rust workspace: new crate uperf-dfps at rust/uperf-dfps/, builds as
   libuperf_dfps.a, linked into the existing binary — OR — dfps-rs as a
   sub-binary of uperf-cli style? Or as a separate binary?
2. C++: deleting cpp/dfps means changing CMakeLists.txt and removing the
   static lib link from uperf.
3. build.sh: build_webui already verifies the WebUI bundle — same gate for
   the new daemon?
4. NOTICE deletion: drop spdlog + scnlib + dfps entries.

## Required output

A short list of build.sh patches + CMakeLists patches + the workspace
file structure.
