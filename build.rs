// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

fn main() {
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/resources.xml",
        "finder.gresource",
    );
}
