<!-- SPDX-FileCopyrightText: 2026 Slavick
     SPDX-License-Identifier: MIT -->

# Localization

English is the source language. All app labels use gettext; filesystem names are
never translated. GTK/GIO supply their own system translations.

Extract the template after changing UI strings:

```sh
bash tools/update-translations.sh
msginit --input=po/finder-gnome.pot --locale=ru --output-file=po/ru.po
msgfmt --check po/ru.po -o /tmp/finder-gnome-ru.mo
```

Install a compiled catalog as
`<locale-dir>/<language>/LC_MESSAGES/finder-gnome.mo`. The default locale directory
is `/usr/share/locale`; for a development build use:

```sh
FINDER_GNOME_LOCALEDIR="$PWD/target/locale" LANGUAGE=ru cargo run --locked
```

Keep named placeholders such as `{count}`, `{source}`, `{destination}` unchanged.
Use gettext plural forms for counts. Mark labels stored in static arrays with
`mark("English label")`, then call `tr(label)` when building widgets. Do not join
translated sentence fragments. No application translations ship yet; the default UI is English.
