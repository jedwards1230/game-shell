find_package(Qt6 6.5 REQUIRED COMPONENTS Test QuickTest)

# --- lane 1: the pure mapping. No display, no Qt GUI, no X. -----------------
qt_add_executable(tst_surfacetags tst_surfacetags.cpp)
target_link_libraries(tst_surfacetags PRIVATE Qt6::Test tvshell-tags)
add_test(NAME surfacetags COMMAND tst_surfacetags)

# --- lane 2: the focus graph + the QML types, offscreen. -------------------
# Imports the SAME TvShell module the shell ships (linked, not copied), so there
# is no drift between what is tested and what runs.
qt_add_executable(tst_qml qml_main.cpp)
target_link_libraries(tst_qml PRIVATE Qt6::QuickTest Qt6::Qml tvshell-qmlplugin)
add_test(NAME qml COMMAND tst_qml -input ${CMAKE_CURRENT_SOURCE_DIR}/qml)
set_tests_properties(qml PROPERTIES ENVIRONMENT
    "QT_QPA_PLATFORM=offscreen;QT_QUICK_BACKEND=software")

# --- lane 3: the ordering assertion, against a real X server. --------------
# Needs an X display, so it is registered only when TV_SHELL_TEST_XVFB names one.
# Same opt-in shape as core/'s atoms_xvfb.rs. The variable is read at CONFIGURE
# time on purpose: a lane that silently registers and then skips itself is
# indistinguishable from a lane that passed, which is the failure mode #469 was
# about. Configure without it and `ctest` reports 2 tests; configure with it and
# it reports 3.
qt_add_executable(tst_premap tst_premap.cpp)
target_link_libraries(tst_premap PRIVATE Qt6::Test Qt6::Quick tvshell-qml PkgConfig::XCB)

if(DEFINED ENV{TV_SHELL_TEST_XVFB} AND NOT "$ENV{TV_SHELL_TEST_XVFB}" STREQUAL "")
    message(STATUS "shell-v2: premap lane ENABLED on DISPLAY=$ENV{TV_SHELL_TEST_XVFB}")
    add_test(NAME premap COMMAND tst_premap)
    set_tests_properties(premap PROPERTIES ENVIRONMENT
        "QT_QPA_PLATFORM=xcb;QT_QUICK_BACKEND=software;DISPLAY=$ENV{TV_SHELL_TEST_XVFB}")
else()
    message(STATUS "shell-v2: premap lane SKIPPED (set TV_SHELL_TEST_XVFB=:99 to enable)")
endif()
