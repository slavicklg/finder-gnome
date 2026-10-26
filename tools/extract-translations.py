#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

"""Use Ubuntu's C xgettext lexer without letting Rust lifetimes hide strings."""
import pathlib
import re
import subprocess
import tempfile
import tomllib

root = pathlib.Path(__file__).resolve().parent.parent
version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
# Preserve strings, character literals and comments verbatim; remove lifetime
# tokens only in code. Our translated strings use ordinary quoted Rust literals,
# whose escaping is compatible with xgettext's C lexer on Ubuntu 24.04.
tokens = re.compile(
    r'"(?:[^"\\]|\\.)*"|//[^\n]*|/\*[\s\S]*?\*/'
    r"|'(?:\\.|[^'\\])'|'[A-Za-z_][A-Za-z_0-9]*"
)

with tempfile.TemporaryDirectory(prefix="finder-gettext-") as directory:
    staging = pathlib.Path(directory)
    (staging / "src").mkdir()
    sources = []
    for path in sorted((root / "src").rglob("*.rs")):
        text = path.read_text()
        text = tokens.sub(
            lambda match: "" if re.fullmatch(r"'[A-Za-z_][A-Za-z_0-9]*", match[0]) else match[0],
            text,
        )
        relative = path.relative_to(root)
        (staging / relative).parent.mkdir(parents=True, exist_ok=True)
        (staging / relative).write_text(text)
        sources.append(str(relative))
    subprocess.run(
        ["xgettext", "--language=C", "--from-code=UTF-8", "--keyword=tr",
         "--keyword=mark", "--keyword=count:1,2", "--keyword=icon_button:2",
         "--add-comments=TRANSLATORS", "--package-name=finder-gnome",
         f"--package-version={version}", "--copyright-holder=Slavick",
         f"--output={root / 'po/finder-gnome.pot'}", *sources],
        cwd=staging, check=True,
    )

    template = root / "po/finder-gnome.pot"
    notice = "# SPDX-FileCopyrightText: 2026 Slavick\n# SPDX-License-Identifier: MIT\n"
    template.write_text(notice + template.read_text().replace("Copyright (C) YEAR Slavick", "Copyright (C) 2026 Slavick"))
