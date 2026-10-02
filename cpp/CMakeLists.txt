# Vendored dfps platform + thirdparty (spdlog is a header-only INTERFACE target, scnlib
# is built here with EXCLUDE_FROM_ALL). These files are unmodified upstream sources:
# see dfps/DFPS_VENDOR.md.
add_subdirectory(dfps/thirdparty thirdparty)

# Portability shim for the vendored scnlib (v1.1.2): include/scn/detail/util.h uses
# std::addressof() without including <memory>, which worked with the NDK r24 libc++
# headers dfps was built against but fails with newer NDK libc++ (r30). Forcing the
# header in keeps the vendored sources byte-identical (no patching of cpp/dfps/**).
if(TARGET scn)
    target_compile_options(scn PRIVATE -include memory)
endif()

# Treat the vendored third-party include roots as system headers. Reason: spdlog 1.9.2's
# bundled fmt declares literal operators as "operator"" _format" (whitespace), which
# clang 21 (NDK r30) warns about; dfps builds with -Werror, so those warnings would break
# the build. System includes are exempt from -Werror and no vendored file is modified.
foreach(dep spdlog scn)
    if(TARGET ${dep})
        get_target_property(_dep_incs ${dep} INTERFACE_INCLUDE_DIRECTORIES)
        if(_dep_incs)
            set_target_properties(${dep} PROPERTIES INTERFACE_SYSTEM_INCLUDE_DIRECTORIES "${_dep_incs}")
        endif()
    endif()
endforeach()

add_subdirectory(uperf)
