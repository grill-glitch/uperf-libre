# T08 — dfps-rs data structures (dynamic_fps → Rust)

## Question

Map dynamic_fps.cpp's rule table (std::map<std::string, FpsRule> with special
pkg names `*` and `-`) to Rust idioms. Decide:

1. HashMap<String, FpsRule> vs BTreeMap? The match is `pkg -> rule` lookups,
   no ordering needed. HashMap is what dfps uses (std::map is just the closest
   no-hashmap std option in C++). Use HashMap.
2. FpsRule is `(int idle, int active)` plus implicit `isUniversal` / `isOffscreen`
   derived from the pkg name. In Rust, make those a single enum or just two
   special match arms? dfps uses string match on `pkgName == UNIVERSIAL_PKG_NAME`
   etc. Decide: enum vs string key.

## Evidence

- source/modules/dynamic_fps.cpp:137-145 (AddRule, isUniversal, isOffscreen)
- source/modules/dynamic_fps.h:21-25 (FpsRule struct)
