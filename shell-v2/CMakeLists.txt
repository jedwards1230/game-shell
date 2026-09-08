# tv-shell v2 shell.
#
# This is the build step v1's shell deliberately does not have. See
# docs/V2_SHELL.md for why it is now unavoidable (pre-map X11 tagging needs C++)
# and what it costs (v2's shell is no longer rsync-deployable).
#
# ONE QML module, one URI. v1 has eleven qmldir registries and 69 relative-dir
# imports; that is the thing this file exists to make impossible. Everything
# under qml/TvShell/ is `import TvShell`, there is no hand-written qmldir, and
# there is no second module to add a type to.
cmake_minimum_required(VERSION 3.21)

project(tv-shell-v2 VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(Qt6 6.5 REQUIRED COMPONENTS Core Gui Qml Quick)
qt_standard_project_setup(REQUIRES 6.5)

# XCB is a hard requirement, not an optional extra: without it there is no
# pre-map tagging and therefore no v2 shell. Failing at configure time is the
# honest outcome.
find_package(PkgConfig REQUIRED)
pkg_check_modules(XCB REQUIRED IMPORTED_TARGET xcb)

# ---------------------------------------------------------------------------
# tvshell-tags — the pure decision layer.
#
# Deliberately its own target with NO Qt GUI and NO X11 in its link line, so its
# unit test is a plain executable that runs anywhere: no display server, no
# platform plugin, no compositor. This is v1's `.pragma library` discipline
# expressed in CMake — if this target ever gains a GUI or X dependency, the split
# has been lost and the test stops being cheap enough to always run.
# ---------------------------------------------------------------------------
add_library(tvshell-tags STATIC src/surfacetags.cpp)
target_include_directories(tvshell-tags PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/src)
set_target_properties(tvshell-tags PROPERTIES POSITION_INDEPENDENT_CODE ON)

# ---------------------------------------------------------------------------
# The single QML module. A library rather than an app-embedded module so the
# QML test executable can import the SAME module the shell ships — no assembled
# copy of the production files, which is how v1's headless suite has to do it.
# ---------------------------------------------------------------------------
qt_add_library(tvshell-qml STATIC)

# Flatten the module's resource paths. Without an alias, `qml/TvShell/Main.qml`
# lands at `:/qt/qml/TvShell/qml/TvShell/Main.qml` — the source layout leaks into
# every import path, and a test that reaches focusGraph.js directly has to spell
# the doubled prefix. With the alias, the module directory on disk and the module
# directory in the resource tree are the same shape: `:/qt/qml/TvShell/<file>`.
foreach(_qmlfile Main.qml FocusRouter.qml FocusSlot.qml focusGraph.js)
    set_source_files_properties(qml/TvShell/${_qmlfile}
        PROPERTIES QT_RESOURCE_ALIAS ${_qmlfile})
endforeach()

qt_add_qml_module(tvshell-qml
    URI TvShell
    VERSION 1.0
    # Must end in the URI's path or qmllint cannot resolve the module and the
    # `all_qmllint` target silently checks nothing useful. CI builds that target.
    OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/qml/TvShell
    QML_FILES
        qml/TvShell/Main.qml
        qml/TvShell/FocusRouter.qml
        qml/TvShell/FocusSlot.qml
    RESOURCES
        qml/TvShell/focusGraph.js
    SOURCES
        src/surface.h
        src/surface.cpp
        src/x11tagger.h
        src/x11tagger.cpp
)

target_link_libraries(tvshell-qml
    PUBLIC Qt6::Gui Qt6::Quick tvshell-tags
    PRIVATE PkgConfig::XCB
)
# ---------------------------------------------------------------------------
# The application.
# ---------------------------------------------------------------------------
qt_add_executable(tv-shell-v2 src/main.cpp)
target_link_libraries(tv-shell-v2 PRIVATE Qt6::Gui Qt6::Quick tvshell-qmlplugin)

include(GNUInstallDirs)
install(TARGETS tv-shell-v2 RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})

# ---------------------------------------------------------------------------
# Tests. Three lanes, in increasing order of what they need to run:
#
#   tags     pure C++, no display, no Qt GUI          — always runs
#   qml      offscreen QML, no display server         — always runs
#   premap   real X server, asserts property-vs-map   — TV_SHELL_TEST_XVFB only
#
# The gating on the third mirrors core/'s TV_SHELL_TEST_XVFB convention, and CI
# sets that variable so the leg actually executes. An ignore-gated leg wired to
# nothing defends nothing (jedwards1230/tv-shell#469).
# ---------------------------------------------------------------------------
option(TVSHELL_V2_TESTS "Build the shell-v2 test lanes" ON)

if(TVSHELL_V2_TESTS)
    enable_testing()
    add_subdirectory(tests)
endif()
